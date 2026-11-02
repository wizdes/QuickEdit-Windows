use crate::{Dip, InputEvent, InputKind, Modifiers, MouseButton, Point};

/// Replay files are UTF-8, one tab-separated record per line. `#` starts a comment.
/// `event<TAB>microseconds<TAB>kind<TAB>fields...` records input.
/// `expect<TAB>name<TAB>value` records a result. Text fields support `\\n`, `\\r`, `\\t`, `\\u{hex}`.
#[derive(Clone, Debug, PartialEq)]
pub enum ReplayStep {
    Event(InputEvent),
    Expect { name: String, value: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayError {
    pub line: usize,
    pub message: String,
}

pub fn parse_replay(source: &str) -> Result<Vec<ReplayStep>, ReplayError> {
    let mut steps = Vec::new();
    for (index, line) in source.lines().enumerate() {
        let line_no = index + 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let parse = || -> Result<ReplayStep, String> {
            let field = |i: usize| {
                fields
                    .get(i)
                    .copied()
                    .ok_or_else(|| format!("missing field {i}"))
            };
            let number = |i: usize| {
                field(i)?
                    .parse::<u64>()
                    .map_err(|_| format!("invalid number in field {i}"))
            };
            let float = |i: usize| {
                field(i)?
                    .parse::<f32>()
                    .map_err(|_| format!("invalid number in field {i}"))
            };
            let point = |i: usize| -> Result<Point, String> {
                Ok(Point {
                    x: Dip(float(i)?),
                    y: Dip(float(i + 1)?),
                })
            };
            let modifiers = |i: usize| -> Result<Modifiers, String> {
                let s = field(i)?;
                if !s.chars().all(|c| "CASW-".contains(c)) {
                    return Err(format!("invalid modifiers in field {i}"));
                }
                Ok(Modifiers {
                    ctrl: s.contains('C'),
                    alt: s.contains('A'),
                    shift: s.contains('S'),
                    win: s.contains('W'),
                })
            };
            let button = |i: usize| -> Result<MouseButton, String> {
                match field(i)? {
                    "left" => Ok(MouseButton::Left),
                    "middle" => Ok(MouseButton::Middle),
                    "right" => Ok(MouseButton::Right),
                    "x1" => Ok(MouseButton::X1),
                    "x2" => Ok(MouseButton::X2),
                    _ => Err(format!("invalid mouse button in field {i}")),
                }
            };
            match field(0)? {
                "expect" => {
                    if fields.len() != 3 {
                        return Err("expect requires name and value".into());
                    }
                    Ok(ReplayStep::Expect {
                        name: field(1)?.into(),
                        value: unescape(field(2)?)?,
                    })
                }
                "event" => {
                    let time_us = number(1)?;
                    let kind_name = field(2)?;
                    let expected_fields = match kind_name {
                        "key_down" | "mouse_down" | "mouse_up" => {
                            6 + usize::from(kind_name != "key_down")
                        }
                        "key_up" | "ime_update" | "mouse_move" | "resize" => 5,
                        "char" | "ime_end" | "dpi" => 4,
                        "ime_start" | "focus" | "blur" => 3,
                        "wheel" => 8,
                        other => return Err(format!("unknown event kind {other}")),
                    };
                    if fields.len() != expected_fields {
                        return Err(format!("{kind_name} requires {expected_fields} fields"));
                    }
                    let kind = match kind_name {
                        "key_down" => InputKind::KeyDown {
                            virtual_key: number(3)?
                                .try_into()
                                .map_err(|_| "virtual key out of range")?,
                            modifiers: modifiers(4)?,
                            repeat: match field(5)? {
                                "true" => true,
                                "false" => false,
                                _ => return Err("key repeat must be true or false".into()),
                            },
                        },
                        "key_up" => InputKind::KeyUp {
                            virtual_key: number(3)?
                                .try_into()
                                .map_err(|_| "virtual key out of range")?,
                            modifiers: modifiers(4)?,
                        },
                        "char" => {
                            let s = unescape(field(3)?)?;
                            let mut chars = s.chars();
                            let c = chars.next().ok_or("empty char")?;
                            if chars.next().is_some() {
                                return Err("char requires one scalar".into());
                            }
                            InputKind::Char(c)
                        }
                        "ime_start" => InputKind::ImeStart,
                        "ime_update" => InputKind::ImeUpdate {
                            text: unescape(field(3)?)?,
                            cursor_utf16: number(4)? as usize,
                        },
                        "ime_end" => InputKind::ImeEnd {
                            text: unescape(field(3)?)?,
                        },
                        "mouse_move" => InputKind::MouseMove { at: point(3)? },
                        "mouse_down" => InputKind::MouseDown {
                            at: point(3)?,
                            button: button(5)?,
                            modifiers: modifiers(6)?,
                        },
                        "mouse_up" => InputKind::MouseUp {
                            at: point(3)?,
                            button: button(5)?,
                            modifiers: modifiers(6)?,
                        },
                        "wheel" => InputKind::Wheel {
                            at: point(3)?,
                            delta_x: float(5)?,
                            delta_y: float(6)?,
                            modifiers: modifiers(7)?,
                        },
                        "focus" => InputKind::Focus,
                        "blur" => InputKind::Blur,
                        "dpi" => InputKind::DpiChange {
                            dpi: number(3)?.try_into().map_err(|_| "dpi out of range")?,
                        },
                        "resize" => InputKind::Resize {
                            width: Dip(float(3)?),
                            height: Dip(float(4)?),
                        },
                        other => return Err(format!("unknown event kind {other}")),
                    };
                    Ok(ReplayStep::Event(InputEvent { time_us, kind }))
                }
                other => Err(format!("unknown record {other}")),
            }
        };
        steps.push(parse().map_err(|message| ReplayError {
            line: line_no,
            message,
        })?);
    }
    Ok(steps)
}

fn unescape(input: &str) -> Result<String, String> {
    let mut chars = input.chars().peekable();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next().ok_or("trailing escape")? {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            '\\' => out.push('\\'),
            'u' => {
                if chars.next() != Some('{') {
                    return Err("expected { after \\u".into());
                }
                let mut hex = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => hex.push(c),
                        None => return Err("unterminated unicode escape".into()),
                    }
                }
                let code = u32::from_str_radix(&hex, 16).map_err(|_| "invalid unicode escape")?;
                out.push(char::from_u32(code).ok_or("invalid unicode scalar")?);
            }
            other => return Err(format!("unknown escape {other}")),
        }
    }
    Ok(out)
}

/// Runs parsed input through a test handler and checks each named result immediately.
pub fn run_replay(
    steps: &[ReplayStep],
    mut event: impl FnMut(&InputEvent),
    mut actual: impl FnMut(&str) -> Option<String>,
) -> Result<(), String> {
    for (index, step) in steps.iter().enumerate() {
        match step {
            ReplayStep::Event(input) => event(input),
            ReplayStep::Expect { name, value } => {
                let got = actual(name)
                    .ok_or_else(|| format!("step {}: unknown result {name}", index + 1))?;
                if &got != value {
                    return Err(format!(
                        "step {}: {name}: expected {value:?}, got {got:?}",
                        index + 1
                    ));
                }
            }
        }
    }
    Ok(())
}
