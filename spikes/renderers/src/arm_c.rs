//! Arm c: a Direct3D 11 waitable flip-model swap chain and a glyph atlas.
//! DirectWrite draws each glyph once into a texture, and a small precompiled
//! shader draws one quad per glyph. d3dcompiler is never loaded.
use crate::atlas::{self, Place, ShelfPacker};
use crate::ffi::{
    self, BlendDesc, Box3d, BufferDesc, Com, GlyphRun, Guid, H, Hr, InputElement, Mapped, Rect,
    TargetBlend, Texture2dDesc, Viewport, check, v,
};
use crate::text::{self, Font, Layout};
use crate::{gpu, vcall};
use std::{collections::BTreeMap, ptr, sync::Arc};

const VERTEX_SHADER: &[u8] = include_bytes!("../shaders/glyph_vs.cso");
const PIXEL_SHADER: &[u8] = include_bytes!("../shaders/glyph_ps.cso");
const MAX_QUADS: usize = (text::COLUMNS as usize) * text::ROWS;
/// A hung swap chain stops the run with an error after this long.
const WAIT_LIMIT_MS: u32 = 1000;
const NATURAL_SYMMETRIC: u32 = 5;
const CLEARTYPE_3X1: u32 = 1;

pub enum Output {
    /// Arm c: the swap chain belongs to the window.
    Window(H),
    /// Arm d: the swap chain will be the content of a DirectComposition visual.
    Composition,
}

/// One quad: clip-space rectangle and atlas rectangle. Matches `glyph.hlsl`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Quad {
    rect: [f32; 4],
    texels: [f32; 4],
}

#[derive(Clone, Copy)]
struct Slot {
    place: Place,
    left: i32,
    top: i32,
    width: u32,
    height: u32,
}

pub struct ArmC {
    font: Arc<Font>,
    layout: Layout,
    pub device: Com,
    context: Com,
    pub swap: Com,
    back: Com,
    rtv: Com,
    waitable: H,
    vertex_shader: Com,
    pixel_shader: Com,
    input_layout: Com,
    blend: Com,
    quads_buffer: Com,
    atlas_texture: Com,
    atlas_view: Com,
    packer: ShelfPacker,
    slots: BTreeMap<u16, Slot>,
    quads: Vec<Quad>,
}
// One thread uses this at a time: arm d builds it on a background thread and
// hands it to the window thread once. `waitable` is an event handle.
unsafe impl Send for ArmC {}

impl ArmC {
    pub fn new(output: Output, font: Arc<Font>, layout: Layout) -> Result<Self, String> {
        let (device, context) = gpu::create_device(0)?;
        let dxgi_device = device.query(&ffi::IID_DXGI_DEVICE, "IDXGIDevice")?;
        let factory = gpu::factory_of(&dxgi_device)?;
        let mut swap = Com::null();
        match output {
            Output::Window(hwnd) => {
                let desc = gpu::swap_desc(
                    layout.width,
                    layout.height,
                    ffi::DXGI_ALPHA_MODE_UNSPECIFIED,
                    ffi::DXGI_FLAG_WAITABLE,
                );
                check(
                    vcall!(
                        factory.0,
                        v::DXF_CREATE_SWAP_CHAIN_HWND,
                        fn(H, H, *const ffi::SwapChainDesc1, H, H, *mut H) -> Hr,
                        device.0,
                        hwnd,
                        &desc,
                        ptr::null_mut(),
                        ptr::null_mut(),
                        swap.slot()
                    ),
                    "CreateSwapChainForHwnd",
                )?;
            }
            Output::Composition => {
                let desc = gpu::swap_desc(
                    layout.width,
                    layout.height,
                    ffi::DXGI_ALPHA_MODE_IGNORE,
                    ffi::DXGI_FLAG_WAITABLE,
                );
                check(
                    vcall!(
                        factory.0,
                        v::DXF_CREATE_SWAP_CHAIN_COMPOSITION,
                        fn(H, *const ffi::SwapChainDesc1, H, *mut H) -> Hr,
                        device.0,
                        &desc,
                        ptr::null_mut(),
                        swap.slot()
                    ),
                    "CreateSwapChainForComposition",
                )?;
            }
        }
        let swap2 = swap.query(&ffi::IID_DXGI_SWAP_CHAIN2, "IDXGISwapChain2")?;
        check(
            vcall!(swap2.0, v::DXS2_SET_MAX_LATENCY, fn(u32) -> Hr, 1),
            "SetMaximumFrameLatency",
        )?;
        let waitable = vcall!(swap2.0, v::DXS2_GET_WAITABLE, fn() -> H);
        if waitable.is_null() {
            return Err("GetFrameLatencyWaitableObject returned nothing".into());
        }
        let mut back = Com::null();
        check(
            vcall!(
                swap.0,
                v::DXS_GET_BUFFER,
                fn(u32, *const Guid, *mut H) -> Hr,
                0,
                &ffi::IID_D3D11_TEXTURE2D,
                back.slot()
            ),
            "GetBuffer",
        )?;
        let mut rtv = Com::null();
        check(
            vcall!(
                device.0,
                v::D3D_CREATE_RTV,
                fn(H, H, *mut H) -> Hr,
                back.0,
                ptr::null_mut(),
                rtv.slot()
            ),
            "CreateRenderTargetView",
        )?;
        let mut vertex_shader = Com::null();
        check(
            vcall!(
                device.0,
                v::D3D_CREATE_VS,
                fn(*const u8, usize, H, *mut H) -> Hr,
                VERTEX_SHADER.as_ptr(),
                VERTEX_SHADER.len(),
                ptr::null_mut(),
                vertex_shader.slot()
            ),
            "CreateVertexShader",
        )?;
        let mut pixel_shader = Com::null();
        check(
            vcall!(
                device.0,
                v::D3D_CREATE_PS,
                fn(*const u8, usize, H, *mut H) -> Hr,
                PIXEL_SHADER.as_ptr(),
                PIXEL_SHADER.len(),
                ptr::null_mut(),
                pixel_shader.slot()
            ),
            "CreatePixelShader",
        )?;
        let element = |name: &'static [u8], offset: u32| InputElement {
            name: name.as_ptr(),
            index: 0,
            format: ffi::DXGI_FORMAT_R32G32B32A32_FLOAT,
            slot: 0,
            offset,
            class: ffi::INPUT_PER_INSTANCE,
            step: 1,
        };
        let elements = [element(b"POS\0", 0), element(b"UV\0", 16)];
        let mut input_layout = Com::null();
        check(
            vcall!(
                device.0,
                v::D3D_CREATE_INPUT_LAYOUT,
                fn(*const InputElement, u32, *const u8, usize, *mut H) -> Hr,
                elements.as_ptr(),
                2,
                VERTEX_SHADER.as_ptr(),
                VERTEX_SHADER.len(),
                input_layout.slot()
            ),
            "CreateInputLayout",
        )?;
        // Black text: destination * (1 - coverage), for each color channel.
        let mut targets = [TargetBlend::default(); 8];
        targets[0] = TargetBlend {
            enable: 1,
            src: ffi::BLEND_ZERO,
            dest: ffi::BLEND_INV_SRC_COLOR,
            op: ffi::BLEND_OP_ADD,
            src_alpha: ffi::BLEND_ZERO,
            dest_alpha: ffi::BLEND_ONE,
            op_alpha: ffi::BLEND_OP_ADD,
            write_mask: 0xf,
        };
        let blend_desc = BlendDesc {
            alpha_to_coverage: 0,
            independent: 0,
            targets,
        };
        let mut blend = Com::null();
        check(
            vcall!(
                device.0,
                v::D3D_CREATE_BLEND,
                fn(*const BlendDesc, *mut H) -> Hr,
                &blend_desc,
                blend.slot()
            ),
            "CreateBlendState",
        )?;
        let buffer_desc = BufferDesc {
            bytes: (MAX_QUADS * size_of::<Quad>()) as u32,
            usage: ffi::USAGE_DYNAMIC,
            bind: ffi::BIND_VERTEX,
            cpu: ffi::CPU_WRITE,
            misc: 0,
            stride: 0,
        };
        let mut quads_buffer = Com::null();
        check(
            vcall!(
                device.0,
                v::D3D_CREATE_BUFFER,
                fn(*const BufferDesc, H, *mut H) -> Hr,
                &buffer_desc,
                ptr::null_mut(),
                quads_buffer.slot()
            ),
            "CreateBuffer",
        )?;
        let side = atlas::atlas_side(layout.cell, layout.line);
        let texture_desc = Texture2dDesc {
            width: side,
            height: side,
            mips: 1,
            array: 1,
            format: ffi::DXGI_FORMAT_R8G8B8A8_UNORM,
            sample_count: 1,
            sample_quality: 0,
            usage: ffi::USAGE_DEFAULT,
            bind: ffi::BIND_SHADER_RESOURCE,
            cpu: 0,
            misc: 0,
        };
        let mut atlas_texture = Com::null();
        check(
            vcall!(
                device.0,
                v::D3D_CREATE_TEXTURE2D,
                fn(*const Texture2dDesc, H, *mut H) -> Hr,
                &texture_desc,
                ptr::null_mut(),
                atlas_texture.slot()
            ),
            "CreateTexture2D(atlas)",
        )?;
        let mut atlas_view = Com::null();
        check(
            vcall!(
                device.0,
                v::D3D_CREATE_SRV,
                fn(H, H, *mut H) -> Hr,
                atlas_texture.0,
                ptr::null_mut(),
                atlas_view.slot()
            ),
            "CreateShaderResourceView",
        )?;
        Ok(ArmC {
            font,
            layout,
            device,
            context,
            swap,
            back,
            rtv,
            waitable,
            vertex_shader,
            pixel_shader,
            input_layout,
            blend,
            quads_buffer,
            atlas_texture,
            atlas_view,
            packer: ShelfPacker::new(side),
            slots: BTreeMap::new(),
            quads: Vec::with_capacity(MAX_QUADS),
        })
    }

    /// Draws a glyph into the atlas once and remembers where it went.
    fn slot(&mut self, glyph: u16) -> Result<Slot, String> {
        if let Some(slot) = self.slots.get(&glyph) {
            return Ok(*slot);
        }
        let advance = 0f32;
        let run = GlyphRun {
            face: self.font.face.0,
            size: self.layout.em_px,
            count: 1,
            indices: &glyph,
            advances: &advance,
            offsets: ptr::null_mut(),
            sideways: 0,
            bidi: 0,
        };
        let mut analysis = Com::null();
        check(
            vcall!(
                self.font.factory.0,
                v::DWF_CREATE_GLYPH_RUN_ANALYSIS,
                fn(*const GlyphRun, f32, H, u32, u32, f32, f32, *mut H) -> Hr,
                &run,
                1.0,
                ptr::null_mut(),
                NATURAL_SYMMETRIC,
                0,
                0.0,
                0.0,
                analysis.slot()
            ),
            "CreateGlyphRunAnalysis",
        )?;
        let mut bounds = Rect::default();
        check(
            vcall!(
                analysis.0,
                v::DWA_GET_BOUNDS,
                fn(u32, *mut Rect) -> Hr,
                CLEARTYPE_3X1,
                &mut bounds
            ),
            "GetAlphaTextureBounds",
        )?;
        let width = (bounds.right - bounds.left).max(0) as u32;
        let height = (bounds.bottom - bounds.top).max(0) as u32;
        let mut slot = Slot {
            place: Place { x: 0, y: 0 },
            left: bounds.left,
            top: bounds.top,
            width,
            height,
        };
        if width > 0 && height > 0 {
            let mut coverage = vec![0u8; (width * height * 3) as usize];
            check(
                vcall!(
                    analysis.0,
                    v::DWA_CREATE_TEXTURE,
                    fn(u32, *const Rect, *mut u8, u32) -> Hr,
                    CLEARTYPE_3X1,
                    &bounds,
                    coverage.as_mut_ptr(),
                    coverage.len() as u32
                ),
                "CreateAlphaTexture",
            )?;
            let mut pixels = Vec::with_capacity((width * height * 4) as usize);
            for [r, g, b] in coverage.as_chunks::<3>().0 {
                pixels.extend_from_slice(&[*r, *g, *b, 255]);
            }
            slot.place = self
                .packer
                .place(width, height)
                .ok_or("the glyph atlas is full")?;
            let target = Box3d {
                left: slot.place.x,
                top: slot.place.y,
                front: 0,
                right: slot.place.x + width,
                bottom: slot.place.y + height,
                back: 1,
            };
            vcall!(
                self.context.0,
                v::CTX_UPDATE_SUBRESOURCE,
                fn(H, u32, *const Box3d, *const u8, u32, u32) -> (),
                self.atlas_texture.0,
                0,
                &target,
                pixels.as_ptr(),
                width * 4,
                0
            );
        }
        self.slots.insert(glyph, slot);
        Ok(slot)
    }

    /// Puts every sample glyph in the atlas ahead of time. Arm d does this on
    /// its background thread, so the switch does not rasterize on the window
    /// thread. Arm c's first frame rasterizes as part of drawing, as it must.
    pub fn prepare(&mut self) -> Result<(), String> {
        for line in text::SAMPLE {
            for glyph in self.font.glyphs(line)? {
                self.slot(glyph)?;
            }
        }
        Ok(())
    }

    fn build_quads(&mut self, typed: &[char]) -> Result<(), String> {
        let layout = self.layout;
        let (w, h) = (layout.width as f32, layout.height as f32);
        self.quads.clear();
        for row in 0..text::ROWS {
            let glyphs = self.font.glyphs(&text::line_text(row, typed))?;
            for (column, glyph) in glyphs.iter().enumerate() {
                let slot = self.slot(*glyph)?;
                if slot.width == 0 || slot.height == 0 {
                    continue;
                }
                let x = text::MARGIN + column as i32 * layout.cell + slot.left;
                let y = layout.baseline(row) + slot.top;
                let (right, bottom) = (x + slot.width as i32, y + slot.height as i32);
                self.quads.push(Quad {
                    rect: [
                        x as f32 / w * 2.0 - 1.0,
                        1.0 - y as f32 / h * 2.0,
                        right as f32 / w * 2.0 - 1.0,
                        1.0 - bottom as f32 / h * 2.0,
                    ],
                    texels: [
                        slot.place.x as f32,
                        slot.place.y as f32,
                        (slot.place.x + slot.width) as f32,
                        (slot.place.y + slot.height) as f32,
                    ],
                });
            }
        }
        if self.quads.len() > MAX_QUADS {
            return Err(format!(
                "{} quads is over the buffer size",
                self.quads.len()
            ));
        }
        Ok(())
    }

    pub fn draw(&mut self, typed: &[char], dump_path: Option<&str>) -> Result<(), String> {
        const WAIT_OBJECT_0: u32 = 0;
        let waited = unsafe { ffi::WaitForSingleObject(self.waitable, WAIT_LIMIT_MS) };
        if waited != WAIT_OBJECT_0 {
            return Err(format!("the swap chain wait returned {waited:#x}"));
        }
        self.build_quads(typed)?;
        let ctx = self.context.0;
        let mut mapped = Mapped {
            data: ptr::null_mut(),
            row_pitch: 0,
            depth_pitch: 0,
        };
        check(
            vcall!(
                ctx,
                v::CTX_MAP,
                fn(H, u32, u32, u32, *mut Mapped) -> Hr,
                self.quads_buffer.0,
                0,
                ffi::MAP_WRITE_DISCARD,
                0,
                &mut mapped
            ),
            "Map(quads)",
        )?;
        unsafe {
            ptr::copy_nonoverlapping(
                self.quads.as_ptr() as *const u8,
                mapped.data,
                self.quads.len() * size_of::<Quad>(),
            );
        }
        vcall!(ctx, v::CTX_UNMAP, fn(H, u32) -> (), self.quads_buffer.0, 0);

        let white = [1f32; 4];
        let viewport = Viewport {
            x: 0.0,
            y: 0.0,
            width: self.layout.width as f32,
            height: self.layout.height as f32,
            min_depth: 0.0,
            max_depth: 1.0,
        };
        let stride = size_of::<Quad>() as u32;
        let offset = 0u32;
        // A flip-model present unbinds the back buffer, so this repeats each frame.
        vcall!(
            ctx,
            v::CTX_OM_SET_TARGETS,
            fn(u32, *const H, H) -> (),
            1,
            &self.rtv.0,
            ptr::null_mut()
        );
        vcall!(
            ctx,
            v::CTX_CLEAR_RTV,
            fn(H, *const f32) -> (),
            self.rtv.0,
            white.as_ptr()
        );
        vcall!(
            ctx,
            v::CTX_RS_SET_VIEWPORTS,
            fn(u32, *const Viewport) -> (),
            1,
            &viewport
        );
        vcall!(ctx, v::CTX_IA_SET_LAYOUT, fn(H) -> (), self.input_layout.0);
        vcall!(
            ctx,
            v::CTX_IA_SET_VB,
            fn(u32, u32, *const H, *const u32, *const u32) -> (),
            0,
            1,
            &self.quads_buffer.0,
            &stride,
            &offset
        );
        vcall!(
            ctx,
            v::CTX_IA_SET_TOPOLOGY,
            fn(u32) -> (),
            ffi::TOPOLOGY_TRIANGLE_STRIP
        );
        vcall!(
            ctx,
            v::CTX_VS_SET_SHADER,
            fn(H, *const H, u32) -> (),
            self.vertex_shader.0,
            ptr::null(),
            0
        );
        vcall!(
            ctx,
            v::CTX_PS_SET_SHADER,
            fn(H, *const H, u32) -> (),
            self.pixel_shader.0,
            ptr::null(),
            0
        );
        vcall!(
            ctx,
            v::CTX_PS_SET_SRV,
            fn(u32, u32, *const H) -> (),
            0,
            1,
            &self.atlas_view.0
        );
        vcall!(
            ctx,
            v::CTX_OM_SET_BLEND,
            fn(H, *const f32, u32) -> (),
            self.blend.0,
            ptr::null(),
            0xffff_ffff
        );
        vcall!(
            ctx,
            v::CTX_DRAW_INSTANCED,
            fn(u32, u32, u32, u32) -> (),
            4,
            self.quads.len() as u32,
            0,
            0
        );

        if let Some(path) = dump_path {
            gpu::dump_texture(
                &self.device,
                &self.context,
                &self.back,
                (self.layout.width, self.layout.height),
                path,
            )?;
        }
        check(
            vcall!(self.swap.0, v::DXS_PRESENT, fn(u32, u32) -> Hr, 1, 0),
            "Present",
        )
    }
}
