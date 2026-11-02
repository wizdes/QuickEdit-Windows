use crate::{Color, Dip, Point, Rect};

#[derive(Clone, Debug, PartialEq)]
pub enum DrawCommand {
    FillRect {
        rect: Rect,
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
    TextRun {
        at: Point,
        text: String,
        font: String,
        color: Color,
    },
    GlyphRun {
        at: Point,
        font_id: u32,
        glyph_ids: Vec<u16>,
        advances: Vec<Dip>,
        offsets: Vec<Point>,
        color: Color,
    },
    Icon {
        at: Rect,
        icon_id: String,
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
        let mut out = String::from("display-list-v1\n");
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
            color: c,
        } => format!(
            "text {} {} {} {}",
            point(at),
            quoted(font),
            color(c),
            quoted(text)
        ),
        DrawCommand::GlyphRun {
            at,
            font_id,
            glyph_ids,
            advances,
            offsets,
            color: c,
        } => format!(
            "glyph {} {} {:?} {:?} {:?} {}",
            point(at),
            font_id,
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
            color: c,
        } => format!("icon {} {} {}", rect(at), quoted(icon_id), color(c)),
        DrawCommand::PushClip(r) => format!("push-clip {}", rect(r)),
        DrawCommand::PopClip => "pop-clip".into(),
        DrawCommand::PushOpacity(a) => format!("push-opacity {a:.3}"),
        DrawCommand::PopOpacity => "pop-opacity".into(),
    }
}
