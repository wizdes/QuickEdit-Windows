// Arm c and arm d draw one instanced quad per glyph with these two shaders.
// The compiled bytes sit next to this file and are embedded in the exe, so the
// spike never loads d3dcompiler. After an edit, rebuild them with fxc from the
// Windows SDK, from this folder:
//   fxc /nologo /T vs_4_0 /E vs_main /O3 /Qstrip_reflect /Qstrip_debug /Fo glyph_vs.cso glyph.hlsl
//   fxc /nologo /T ps_4_0 /E ps_main /O3 /Qstrip_reflect /Qstrip_debug /Fo glyph_ps.cso glyph.hlsl

struct VsIn {
    float4 rect : POS;   // left, top, right, bottom in clip space
    float4 texels : UV;  // left, top, right, bottom in atlas texels
    uint id : SV_VertexID;
};

struct VsOut {
    float4 position : SV_Position;
    float2 texel : TEXCOORD0;
};

VsOut vs_main(VsIn input) {
    VsOut output;
    float right = (input.id & 1) ? 1.0 : 0.0;
    float bottom = (input.id & 2) ? 1.0 : 0.0;
    output.position = float4(
        lerp(input.rect.x, input.rect.z, right),
        lerp(input.rect.y, input.rect.w, bottom),
        0.0,
        1.0);
    output.texel = float2(
        lerp(input.texels.x, input.texels.z, right),
        lerp(input.texels.y, input.texels.w, bottom));
    return output;
}

Texture2D<float4> atlas : register(t0);

// The result is the coverage of each color channel. The blend state turns it
// into black text: destination * (1 - coverage).
float4 ps_main(VsOut input) : SV_Target {
    return atlas.Load(int3(int2(input.texel), 0));
}
