//! Counts bytes, UTF-16 units and line breaks in valid UTF-8.
//!
//! Both routes turn each 32 bytes into bit masks and share `Scan::add`, so the
//! line break rules exist in one place. A break is LF, CR, CRLF as one,
//! U+0085 (`C2 85`), U+2028 (`E2 80 A8`) or U+2029 (`E2 80 A9`).

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Sum {
    pub bytes: usize,
    pub units: usize,
    pub breaks: usize,
    pub first_lf: bool,
    pub last_cr: bool,
}

impl Sum {
    pub fn add(self, b: Self) -> Self {
        if self.bytes == 0 {
            return b;
        }
        if b.bytes == 0 {
            return self;
        }
        Self {
            bytes: self.bytes + b.bytes,
            units: self.units + b.units,
            breaks: self.breaks + b.breaks - usize::from(self.last_cr && b.first_lf),
            first_lf: self.first_lf,
            last_cr: b.last_cr,
        }
    }
}

/// Each bit stands for one byte of a group of up to 32 bytes.
#[derive(Default)]
struct Masks {
    cr: u32,
    lf: u32,
    c2: u32,
    x85: u32,
    e2: u32,
    x80: u32,
    a8_a9: u32,
    continuation: u32,
    four_byte: u32,
}

/// The bits that a sequence needs from the group before this one.
#[derive(Default)]
struct Scan {
    units: usize,
    breaks: usize,
    previous_cr: u32,
    previous_c2: u32,
    previous_e2: u32,
    previous_x80: u32,
}

impl Scan {
    #[inline(always)]
    fn add(&mut self, m: &Masks, len: u32) {
        self.units += (len - m.continuation.count_ones() + m.four_byte.count_ones()) as usize;
        let after_cr = (m.cr << 1) | self.previous_cr;
        let next_line = ((m.c2 << 1) | self.previous_c2) & m.x85;
        let separator =
            ((m.e2 << 2) | self.previous_e2) & ((m.x80 << 1) | self.previous_x80) & m.a8_a9;
        self.breaks += (m.cr.count_ones()
            + (m.lf & !after_cr).count_ones()
            + next_line.count_ones()
            + separator.count_ones()) as usize;
        self.previous_cr = m.cr >> 31;
        self.previous_c2 = m.c2 >> 31;
        self.previous_e2 = m.e2 >> 30;
        self.previous_x80 = m.x80 >> 31;
    }
    /// A group of 32 plain ASCII bytes with no break in it.
    #[inline(always)]
    fn add_plain_group(&mut self) {
        self.units += 32;
        self.previous_cr = 0;
        self.previous_c2 = 0;
        self.previous_e2 = 0;
        self.previous_x80 = 0;
    }
    fn finish(self, s: &str) -> Sum {
        Sum {
            bytes: s.len(),
            units: self.units,
            breaks: self.breaks,
            first_lf: s.starts_with('\n'),
            last_cr: s.ends_with('\r'),
        }
    }
}

fn scalar_masks(group: &[u8]) -> Masks {
    let mut m = Masks::default();
    for (i, &b) in group.iter().enumerate() {
        let bit = 1u32 << i;
        match b {
            b'\r' => m.cr |= bit,
            b'\n' => m.lf |= bit,
            0xC2 => m.c2 |= bit,
            0xE2 => m.e2 |= bit,
            0xA8 | 0xA9 => m.a8_a9 |= bit,
            0x85 => m.x85 |= bit,
            0x80 => m.x80 |= bit,
            _ => {}
        }
        if b & 0xC0 == 0x80 {
            m.continuation |= bit;
        }
        if b >= 0xF0 {
            m.four_byte |= bit;
        }
    }
    m
}

pub(crate) fn measure(s: &str) -> Sum {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the runtime feature check above guards every AVX2 instruction.
        return unsafe { measure_avx2(s) };
    }
    measure_plain(s)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn measure_avx2(s: &str) -> Sum {
    use std::arch::x86_64::*;
    let bytes = s.as_bytes();
    let of = |b: u8| _mm256_set1_epi8(b as i8);
    let (cr, lf, c2, x85, e2, x80, a9) = (
        of(b'\r'),
        of(b'\n'),
        of(0xC2),
        of(0x85),
        of(0xE2),
        of(0x80),
        of(0xA9),
    );
    let (one, top_two, f0) = (of(1), of(0xC0), of(0xF0));
    let mut scan = Scan::default();
    let (groups, tail) = bytes.as_chunks::<32>();
    for group in groups {
        // SAFETY: `group` is exactly 32 readable bytes.
        let block = unsafe { _mm256_loadu_si256(group.as_ptr().cast()) };
        let mask = |x: __m256i| _mm256_movemask_epi8(x) as u32;
        let mut m = Masks {
            cr: mask(_mm256_cmpeq_epi8(block, cr)),
            lf: mask(_mm256_cmpeq_epi8(block, lf)),
            ..Masks::default()
        };
        if mask(block) != 0 {
            m.c2 = mask(_mm256_cmpeq_epi8(block, c2));
            m.x85 = mask(_mm256_cmpeq_epi8(block, x85));
            m.e2 = mask(_mm256_cmpeq_epi8(block, e2));
            m.x80 = mask(_mm256_cmpeq_epi8(block, x80));
            m.a8_a9 = mask(_mm256_cmpeq_epi8(_mm256_or_si256(block, one), a9));
            m.continuation = mask(_mm256_cmpeq_epi8(_mm256_and_si256(block, top_two), x80));
            m.four_byte = mask(_mm256_cmpeq_epi8(_mm256_max_epu8(block, f0), block));
        }
        scan.add(&m, 32);
    }
    scan.add(&scalar_masks(tail), tail.len() as u32);
    scan.finish(s)
}

fn measure_plain(s: &str) -> Sum {
    let mut scan = Scan::default();
    let (groups, tail) = s.as_bytes().as_chunks::<32>();
    for group in groups {
        let plain = group.iter().all(|&b| b < 0x80 && b != b'\r' && b != b'\n');
        if plain {
            scan.add_plain_group();
        } else {
            scan.add(&scalar_masks(group), 32);
        }
    }
    scan.add(&scalar_masks(tail), tail.len() as u32);
    scan.finish(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The slow, obvious rule that both fast routes must match.
    fn slow(s: &str) -> Sum {
        let mut breaks = 0;
        let mut previous_cr = false;
        for c in s.chars() {
            if matches!(c, '\r' | '\u{85}' | '\u{2028}' | '\u{2029}') || (c == '\n' && !previous_cr)
            {
                breaks += 1;
            }
            previous_cr = c == '\r';
        }
        Sum {
            bytes: s.len(),
            units: s.encode_utf16().count(),
            breaks,
            first_lf: s.starts_with('\n'),
            last_cr: s.ends_with('\r'),
        }
    }

    fn all_routes(s: &str) {
        assert_eq!(measure_plain(s), slow(s), "plain route on {s:?}");
        assert_eq!(measure(s), slow(s), "default route on {s:?}");
    }

    #[test]
    fn crlf_inside_a_block_counts_once() {
        let mut s = "x".repeat(10);
        s.push_str("\r\n");
        s.push_str(&"x".repeat(40));
        all_routes(&s);
        assert_eq!(measure(&s).breaks, 1);
    }

    #[test]
    fn crlf_across_a_block_edge_counts_once() {
        for pad in [31, 63, 95] {
            let s = format!("{}\r\n{}", "x".repeat(pad), "y".repeat(40));
            all_routes(&s);
            assert_eq!(measure(&s).breaks, 1, "pad {pad}");
        }
    }

    #[test]
    fn a_cr_before_a_plain_group_does_not_join_a_later_lf() {
        // CR ends group one, group two has no breaks, and LF starts group three.
        let s = format!("{}\r{}\n{}", "a".repeat(31), "b".repeat(32), "c".repeat(40));
        all_routes(&s);
        assert_eq!(measure(&s).breaks, 2);
        assert_eq!(measure_plain(&s).breaks, 2);
    }

    #[test]
    fn multi_byte_breaks_across_a_block_edge_count_once() {
        for pad in 28..=33 {
            for sep in ["\u{85}", "\u{2028}", "\u{2029}"] {
                let s = format!("{}{sep}{}", "x".repeat(pad), "y".repeat(40));
                all_routes(&s);
                assert_eq!(measure(&s).breaks, 1, "pad {pad}, {sep:?}");
            }
        }
    }

    #[test]
    fn look_alike_bytes_are_not_breaks() {
        // U+2030 is E2 80 B0, U+2020 is E2 80 A0, U+00A8 is C2 A8, and U+0085's
        // last byte also ends U+0145 (C5 85) and U+3085 (E3 82 85).
        all_routes("\u{2030}\u{2020}\u{a8}\u{145}\u{3085}");
        assert_eq!(measure("\u{2030}\u{2020}\u{a8}\u{145}\u{3085}").breaks, 0);
    }

    #[test]
    fn random_text_matches_the_slow_rule_at_every_length_and_alignment() {
        let parts = [
            "x",
            "yz",
            "\r",
            "\n",
            "\r\n",
            "é",
            "語",
            "💡",
            "\u{85}",
            "\u{2028}",
            "\u{2029}",
            "\u{145}",
            "\u{2030}",
            "                ",
        ];
        let mut rng = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng as usize
        };
        for _ in 0..20_000 {
            let mut s = String::new();
            for _ in 0..next() % 60 {
                s.push_str(parts[next() % parts.len()]);
            }
            all_routes(&s);
        }
    }

    #[test]
    fn sum_add_joins_crlf_and_keeps_the_outer_flags() {
        let joined = measure("a\r").add(measure("\nb"));
        assert_eq!(joined, slow("a\r\nb"));
        assert_eq!(measure("").add(measure("\n")), slow("\n"));
        assert_eq!(measure("\r").add(measure("")), slow("\r"));
    }
}
