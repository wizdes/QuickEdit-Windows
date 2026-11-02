//! Writes a CID-keyed CFF font (Adobe Technical Note 5176) from outlines, and writes
//! its charstrings (Note 5177). Each CID is one glyph, and CID 0 is the empty `.notdef`.

/// One figure of a glyph, in units of 1/1000 em, with y going up.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Figure {
    pub start: (f32, f32),
    pub segments: Vec<Segment>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Segment {
    Line((f32, f32)),
    Curve((f32, f32), (f32, f32), (f32, f32)),
}

/// Charstring operands are integers here. A CFF stack holds 48, so operators repeat
/// before that.
const MOST_ARGUMENTS: usize = 48;

fn push_number(out: &mut Vec<u8>, value: i32) {
    match value {
        -107..=107 => out.push((value + 139) as u8),
        108..=1131 => {
            let rest = value - 108;
            out.push((rest / 256 + 247) as u8);
            out.push((rest % 256) as u8);
        }
        -1131..=-108 => {
            let rest = -value - 108;
            out.push((rest / 256 + 251) as u8);
            out.push((rest % 256) as u8);
        }
        _ => {
            let clamped = value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
            out.push(28);
            out.extend_from_slice(&clamped.to_be_bytes());
        }
    }
}

/// Moves are worked out from rounded places, not added up, so rounding cannot drift.
struct Pen {
    at: (i32, i32),
}
impl Pen {
    fn delta(&mut self, to: (f32, f32)) -> (i32, i32) {
        // One move holds at most an i16, as `push_number` writes it. The pen records where
        // the move really ends, so a clamped move does not shift the rest of the glyph.
        let limit = |value: i32| value.clamp(i32::from(i16::MIN), i32::from(i16::MAX));
        let delta = (
            limit(to.0.round() as i32 - self.at.0),
            limit(to.1.round() as i32 - self.at.1),
        );
        self.at = (self.at.0 + delta.0, self.at.1 + delta.1);
        delta
    }
}

/// The Type 2 charstring of a glyph. A glyph with no figures is empty, like a space.
pub fn charstring(figures: &[Figure]) -> Vec<u8> {
    const RMOVETO: u8 = 21;
    const RLINETO: u8 = 5;
    const RRCURVETO: u8 = 8;
    const ENDCHAR: u8 = 14;
    let mut out = Vec::new();
    let mut pen = Pen { at: (0, 0) };
    for figure in figures {
        let (dx, dy) = pen.delta(figure.start);
        push_number(&mut out, dx);
        push_number(&mut out, dy);
        out.push(RMOVETO);
        let mut held = 0;
        let mut operator = 0;
        let flush = |out: &mut Vec<u8>, held: &mut usize, operator: u8| {
            if *held > 0 {
                out.push(operator);
                *held = 0;
            }
        };
        for segment in &figure.segments {
            let (next, count) = match segment {
                Segment::Line(_) => (RLINETO, 2),
                Segment::Curve(..) => (RRCURVETO, 6),
            };
            if next != operator || held + count > MOST_ARGUMENTS {
                flush(&mut out, &mut held, operator);
                operator = next;
            }
            match *segment {
                Segment::Line(to) => {
                    let (dx, dy) = pen.delta(to);
                    push_number(&mut out, dx);
                    push_number(&mut out, dy);
                }
                Segment::Curve(a, b, c) => {
                    for point in [a, b, c] {
                        let (dx, dy) = pen.delta(point);
                        push_number(&mut out, dx);
                        push_number(&mut out, dy);
                    }
                }
            }
            held += count;
        }
        flush(&mut out, &mut held, operator);
    }
    out.push(ENDCHAR);
    out
}

fn write_index(out: &mut Vec<u8>, items: &[&[u8]]) {
    out.extend_from_slice(&(items.len() as u16).to_be_bytes());
    if items.is_empty() {
        return;
    }
    let total: usize = items.iter().map(|item| item.len()).sum();
    let size: usize = match total + 1 {
        0..=0xFF => 1,
        0x100..=0xFFFF => 2,
        0x1_0000..=0xFF_FFFF => 3,
        _ => 4,
    };
    out.push(size as u8);
    let mut offset = 1usize;
    let put = |out: &mut Vec<u8>, value: usize| {
        out.extend_from_slice(&value.to_be_bytes()[8 - size..]);
    };
    put(out, offset);
    for item in items {
        offset += item.len();
        put(out, offset);
    }
    for item in items {
        out.extend_from_slice(item);
    }
}

fn dict_int(out: &mut Vec<u8>, value: i32) {
    // The 5-byte form, so that offsets do not change the dictionary's size.
    out.push(29);
    out.extend_from_slice(&value.to_be_bytes());
}

/// The font file. `glyphs[0]` must be `.notdef`.
pub fn build(name: &str, glyphs: &[&[u8]], bbox: [i32; 4]) -> Result<Vec<u8>, String> {
    if glyphs.is_empty() || glyphs.len() > 65535 {
        return Err(format!(
            "A font in this document needs {} glyphs, and a font holds at most 65,535.",
            glyphs.len()
        ));
    }
    const SID_ADOBE: i32 = 391;
    const SID_IDENTITY: i32 = 392;

    let mut head = vec![1u8, 0, 4, 4];
    write_index(&mut head, &[name.as_bytes()]);

    let top_dict = |charset: i32, select: i32, chars: i32, array: i32| {
        let mut dict = Vec::new();
        dict_int(&mut dict, SID_ADOBE);
        dict_int(&mut dict, SID_IDENTITY);
        dict_int(&mut dict, 0);
        dict.extend_from_slice(&[12, 30]); // ROS comes first.
        for value in bbox {
            dict_int(&mut dict, value);
        }
        dict.push(5); // FontBBox
        dict_int(&mut dict, glyphs.len() as i32);
        dict.extend_from_slice(&[12, 34]); // CIDCount
        dict_int(&mut dict, charset);
        dict.push(15);
        dict_int(&mut dict, select);
        dict.extend_from_slice(&[12, 37]);
        dict_int(&mut dict, chars);
        dict.push(17);
        dict_int(&mut dict, array);
        dict.extend_from_slice(&[12, 36]);
        dict
    };
    let mut strings = Vec::new();
    write_index(&mut strings, &[b"Adobe", b"Identity"]);
    let mut global = Vec::new();
    write_index(&mut global, &[]);

    // Charset format 2: every CID from 1 is named in order, with SIDs that stand for CIDs.
    // A font with only `.notdef` names nothing, so it has no range.
    let mut charset = vec![2u8];
    if glyphs.len() > 1 {
        charset.extend_from_slice(&1u16.to_be_bytes());
        let left = (glyphs.len() as u16).saturating_sub(2);
        charset.extend_from_slice(&left.to_be_bytes());
    }

    // FDSelect format 3: all glyphs use font dictionary 0.
    let mut select = vec![3u8];
    select.extend_from_slice(&1u16.to_be_bytes());
    select.extend_from_slice(&0u16.to_be_bytes());
    select.push(0);
    select.extend_from_slice(&(glyphs.len() as u16).to_be_bytes());

    let mut chars = Vec::new();
    write_index(&mut chars, glyphs);

    let private = {
        let mut dict = Vec::new();
        dict_int(&mut dict, 0);
        dict.push(20); // defaultWidthX. The PDF's /W gives the real widths.
        dict_int(&mut dict, 0);
        dict.push(21); // nominalWidthX
        dict
    };

    // The sizes are the same whatever the offsets are, so a first pass measures.
    let mut top_index = Vec::new();
    write_index(&mut top_index, &[&top_dict(0, 0, 0, 0)]);
    let charset_at = head.len() + top_index.len() + strings.len() + global.len();
    let select_at = charset_at + charset.len();
    let chars_at = select_at + select.len();
    let array_at = chars_at + chars.len();

    let font_dict = |private_at: i32| {
        let mut dict = Vec::new();
        dict_int(&mut dict, private.len() as i32);
        dict_int(&mut dict, private_at);
        dict.push(18);
        dict
    };
    let mut array = Vec::new();
    write_index(&mut array, &[&font_dict(0)]);
    let private_at = array_at + array.len();
    array.clear();
    write_index(&mut array, &[&font_dict(private_at as i32)]);

    top_index.clear();
    write_index(
        &mut top_index,
        &[&top_dict(
            charset_at as i32,
            select_at as i32,
            chars_at as i32,
            array_at as i32,
        )],
    );

    let mut out = head;
    out.extend_from_slice(&top_index);
    out.extend_from_slice(&strings);
    out.extend_from_slice(&global);
    out.extend_from_slice(&charset);
    out.extend_from_slice(&select);
    out.extend_from_slice(&chars);
    out.extend_from_slice(&array);
    out.extend_from_slice(&private);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The numbers of a charstring, with no operators.
    fn numbers(data: &[u8]) -> Vec<i32> {
        let mut out = Vec::new();
        let mut at = 0;
        while at < data.len() {
            let byte = data[at];
            at += 1;
            match byte {
                28 => {
                    out.push(i32::from(i16::from_be_bytes([data[at], data[at + 1]])));
                    at += 2;
                }
                32..=246 => out.push(i32::from(byte) - 139),
                247..=250 => {
                    out.push((i32::from(byte) - 247) * 256 + i32::from(data[at]) + 108);
                    at += 1;
                }
                251..=254 => {
                    out.push(-(i32::from(byte) - 251) * 256 - i32::from(data[at]) - 108);
                    at += 1;
                }
                _ => {}
            }
        }
        out
    }

    #[test]
    fn a_move_too_long_for_one_number_does_not_shift_the_rest() {
        let figure = Figure {
            start: (40_000.0, 0.0),
            segments: vec![Segment::Line((40_000.0, 100.0))],
        };
        // The first move stops at 32,767, and the next one makes up the rest of the way.
        assert_eq!(numbers(&charstring(&[figure])), vec![32_767, 0, 7_233, 100]);
    }

    #[test]
    fn a_font_with_only_notdef_has_no_charset_range() {
        let empty = charstring(&[]);
        let font = build("X", &[&empty], [0, 0, 0, 0]).unwrap();
        let report = crate::check::read_cff_for_test(&font).unwrap();
        assert_eq!(report, 1);
        let two = build("X", &[&empty, &empty], [0, 0, 0, 0]).unwrap();
        assert_eq!(crate::check::read_cff_for_test(&two).unwrap(), 2);
    }
}
