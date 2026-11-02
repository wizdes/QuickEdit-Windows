use crate::{Color, Dip, Point, Rect};

#[derive(Clone, Debug, PartialEq)]
pub enum DrawCommand {
    FillRect {
        rect: Rect,
        color: Color,
    },
    FillRects {
        rects: Vec<Rect>,
        color: Color,
    },
    StrokeRect {
        rect: Rect,
        radius: Dip,
        width: Dip,
        color: Color,
    },
    WavyUnderline {
        from: Point,
        to: Point,
        color: Color,
    },
    RoundedRect {
        rect: Rect,
        radius: Dip,
        color: Color,
    },
    Line {
        from: Point,
        to: Point,
        width: Dip,
        color: Color,
    },
    /// `at` is the start of the text on its baseline, the same as `GlyphRun`. To
    /// center text in a box, use `TextMeasurer`: `at.y` is the box top, plus half
    /// of the box height minus `TextMetrics::height`, plus `TextMetrics::baseline`.
    TextRun {
        at: Point,
        text: String,
        font: String,
        font_size: Dip,
        color: Color,
    },
    /// `at` is the start of the glyph's baseline. `font_em_size` is the size to
    /// draw the glyph ids at.
    GlyphRun {
        at: Point,
        bidi_level: u8,
        font_id: u32,
        font_em_size: Dip,
        glyph_ids: Vec<u16>,
        advances: Vec<Dip>,
        offsets: Vec<Point>,
        color: Color,
    },
    /// `at` is the start of the icon on its baseline, the same as `TextRun`.
    Icon {
        at: Point,
        icon_id: String,
        font_size: Dip,
        color: Color,
    },
    PushClip(Rect),
    PopClip,
    PushOpacity(f32),
    PopOpacity,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DisplayList {
    pub commands: Vec<DrawCommand>,
}

impl DisplayList {
    pub fn push(&mut self, command: DrawCommand) {
        self.commands.push(command);
    }
    /// Versioned, stable text for saved replay expectations. Floats use fixed 3 decimals.
    pub fn to_stable_text(&self) -> String {
        let mut out = String::from("display-list-v4\n");
        for command in &self.commands {
            out.push_str(&format_command(command));
            out.push('\n');
        }
        out
    }
}

fn rect(r: &Rect) -> String {
    format!(
        "{:.3},{:.3},{:.3},{:.3}",
        r.x.0, r.y.0, r.width.0, r.height.0
    )
}
fn point(p: &Point) -> String {
    format!("{:.3},{:.3}", p.x.0, p.y.0)
}
fn color(c: &Color) -> String {
    format!("#{:02x}{:02x}{:02x}{:02x}", c.r, c.g, c.b, c.a)
}
fn quoted(s: &str) -> String {
    format!("{s:?}")
}
fn format_command(c: &DrawCommand) -> String {
    match c {
        DrawCommand::FillRect { rect: r, color: c } => format!("fill {} {}", rect(r), color(c)),
        DrawCommand::FillRects { rects, color: c } => format!(
            "fill-rects [{}] {}",
            rects.iter().map(rect).collect::<Vec<_>>().join(";"),
            color(c)
        ),
        DrawCommand::StrokeRect {
            rect: r,
            radius,
            width,
            color: c,
        } => format!(
            "stroke-rect {} {:.3} {:.3} {}",
            rect(r),
            radius.0,
            width.0,
            color(c)
        ),
        DrawCommand::WavyUnderline { from, to, color: c } => {
            format!("wavy-underline {} {} {}", point(from), point(to), color(c))
        }
        DrawCommand::RoundedRect {
            rect: r,
            radius,
            color: c,
        } => format!("round {} {:.3} {}", rect(r), radius.0, color(c)),
        DrawCommand::Line {
            from,
            to,
            width,
            color: c,
        } => format!(
            "line {} {} {:.3} {}",
            point(from),
            point(to),
            width.0,
            color(c)
        ),
        DrawCommand::TextRun {
            at,
            text,
            font,
            font_size,
            color: c,
        } => format!(
            "text {} {} {:.3} {} {}",
            point(at),
            quoted(font),
            font_size.0,
            color(c),
            quoted(text)
        ),
        DrawCommand::GlyphRun {
            at,
            bidi_level,
            font_id,
            font_em_size,
            glyph_ids,
            advances,
            offsets,
            color: c,
        } => format!(
            "glyph {} {} {} {:.3} {:?} {:?} {:?} {}",
            point(at),
            bidi_level,
            font_id,
            font_em_size.0,
            glyph_ids,
            advances
                .iter()
                .map(|x| format!("{:.3}", x.0))
                .collect::<Vec<_>>(),
            offsets.iter().map(point).collect::<Vec<_>>(),
            color(c)
        ),
        DrawCommand::Icon {
            at,
            icon_id,
            font_size,
            color: c,
        } => format!(
            "icon {} {} {:.3} {}",
            point(at),
            quoted(icon_id),
            font_size.0,
            color(c)
        ),
        DrawCommand::PushClip(r) => format!("push-clip {}", rect(r)),
        DrawCommand::PopClip => "pop-clip".into(),
        DrawCommand::PushOpacity(a) => format!("push-opacity {a:.3}"),
        DrawCommand::PopOpacity => "pop-opacity".into(),
    }
}
