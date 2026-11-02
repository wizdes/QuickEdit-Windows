//! A small zlib compressor (RFC 1950 and 1951) for PDF's FlateDecode, with fixed Huffman
//! codes and a short hash chain. It trades some size for speed, and it keeps text pages
//! and fonts about 3 to 5 times smaller than plain.

const WINDOW: usize = 32 * 1024;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const HASH_BITS: usize = 15;
const CHAIN_LIMIT: usize = 8;

struct Bits {
    out: Vec<u8>,
    held: u64,
    count: u32,
}
impl Bits {
    /// Adds `length` bits, first bit lowest, as deflate packs them.
    #[inline]
    fn put(&mut self, value: u32, length: u32) {
        self.held |= u64::from(value) << self.count;
        self.count += length;
        while self.count >= 8 {
            self.out.push(self.held as u8);
            self.held >>= 8;
            self.count -= 8;
        }
    }
    /// Huffman codes go in with their first bit highest.
    #[inline]
    fn code(&mut self, code: u32, length: u32) {
        self.put(code.reverse_bits() >> (32 - length), length);
    }
    #[inline]
    fn literal_or_length(&mut self, symbol: u32) {
        match symbol {
            0..=143 => self.code(0x30 + symbol, 8),
            144..=255 => self.code(0x190 + symbol - 144, 9),
            256..=279 => self.code(symbol - 256, 7),
            _ => self.code(0xC0 + symbol - 280, 8),
        }
    }
    fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            self.out.push(self.held as u8);
        }
        self.out
    }
}

const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

/// The hash table and chain, kept between calls, so that a page costs no large allocation.
pub struct Compressor {
    head: Vec<u32>,
    prev: Vec<u32>,
}
impl Default for Compressor {
    fn default() -> Self {
        Self {
            head: vec![0; 1 << HASH_BITS],
            prev: vec![0; WINDOW],
        }
    }
}

impl Compressor {
    /// `data` in a zlib wrapper.
    pub fn compress(&mut self, data: &[u8]) -> Vec<u8> {
        let mut bits = Bits {
            out: Vec::with_capacity(data.len() / 3 + 16),
            held: 0,
            count: 0,
        };
        bits.out.extend_from_slice(&[0x78, 0x01]);
        // One final block with fixed codes.
        bits.put(1, 1);
        bits.put(1, 2);
        self.head.fill(0);
        let mut at = 0;
        while at < data.len() {
            let (length, distance) = self.best_match(data, at);
            if length >= MIN_MATCH {
                let index = LENGTH_BASE.partition_point(|&b| usize::from(b) <= length) - 1;
                // Length 258 has its own code, past the one whose range it would end.
                bits.literal_or_length(257 + index as u32);
                bits.put(
                    (length - usize::from(LENGTH_BASE[index])) as u32,
                    u32::from(LENGTH_EXTRA[index]),
                );
                let slot = DISTANCE_BASE.partition_point(|&b| usize::from(b) <= distance) - 1;
                bits.code(slot as u32, 5);
                bits.put(
                    (distance - usize::from(DISTANCE_BASE[slot])) as u32,
                    u32::from(DISTANCE_EXTRA[slot]),
                );
                for position in at..at + length {
                    self.remember(data, position);
                }
                at += length;
            } else {
                bits.literal_or_length(u32::from(data[at]));
                self.remember(data, at);
                at += 1;
            }
        }
        bits.literal_or_length(256);
        let mut out = bits.finish();
        out.extend_from_slice(&adler32(data).to_be_bytes());
        out
    }
    #[inline]
    fn hash(data: &[u8], at: usize) -> usize {
        let value =
            u32::from(data[at]) | (u32::from(data[at + 1]) << 8) | (u32::from(data[at + 2]) << 16);
        (value.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
    }
    #[inline]
    fn remember(&mut self, data: &[u8], at: usize) {
        if at + MIN_MATCH <= data.len() {
            let hash = Self::hash(data, at);
            self.prev[at % WINDOW] = self.head[hash];
            // Position plus one, so that zero means none.
            self.head[hash] = at as u32 + 1;
        }
    }
    #[inline]
    fn best_match(&self, data: &[u8], at: usize) -> (usize, usize) {
        if at + MIN_MATCH > data.len() {
            return (0, 0);
        }
        let most = (data.len() - at).min(MAX_MATCH);
        let (mut best, mut distance) = (0, 0);
        let mut candidate = self.head[Self::hash(data, at)] as usize;
        let mut tries = 0;
        while candidate > 0 && tries < CHAIN_LIMIT {
            let start = candidate - 1;
            if start >= at || at - start > WINDOW - 1 {
                break;
            }
            let mut length = 0;
            while length < most && data[start + length] == data[at + length] {
                length += 1;
            }
            if length > best {
                best = length;
                distance = at - start;
                if best == most {
                    break;
                }
            }
            let next = self.prev[start % WINDOW] as usize;
            if next >= candidate {
                break;
            }
            candidate = next;
            tries += 1;
        }
        (best, distance)
    }
}

/// Reads what `compress` writes, and any other zlib stream with fixed or dynamic codes.
/// The structure checker uses it. Bad input gives an error, never a panic.
pub fn inflate(input: &[u8]) -> Result<Vec<u8>, String> {
    let body = input.get(2..).ok_or("the zlib header is cut off")?;
    let mut reader = BitReader {
        data: body,
        at: 0,
        held: 0,
        count: 0,
    };
    let mut out = Vec::with_capacity(input.len() * 3);
    loop {
        let last = reader.bits(1)? == 1;
        match reader.bits(2)? {
            0 => {
                reader.held = 0;
                reader.count = 0;
                let length = usize::from(reader.byte()?) | (usize::from(reader.byte()?) << 8);
                reader.byte()?;
                reader.byte()?;
                for _ in 0..length {
                    out.push(reader.byte()?);
                }
            }
            1 => {
                let mut lengths = [8u8; 288];
                lengths[144..256].fill(9);
                lengths[256..280].fill(7);
                let literal = Huffman::new(&lengths)?;
                let distance = Huffman::new(&[5u8; 30])?;
                inflate_block(&mut reader, &literal, &distance, &mut out)?;
            }
            2 => {
                let (literal, distance) = read_dynamic_tables(&mut reader)?;
                inflate_block(&mut reader, &literal, &distance, &mut out)?;
            }
            _ => return Err("a deflate block has a bad type".into()),
        }
        if last {
            return Ok(out);
        }
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    at: usize,
    held: u32,
    count: u32,
}
impl BitReader<'_> {
    fn byte(&mut self) -> Result<u8, String> {
        let byte = *self
            .data
            .get(self.at)
            .ok_or("a deflate stream is cut off")?;
        self.at += 1;
        Ok(byte)
    }
    fn bits(&mut self, wanted: u32) -> Result<u32, String> {
        while self.count < wanted {
            self.held |= u32::from(self.byte()?) << self.count;
            self.count += 8;
        }
        let value = self.held & ((1u32 << wanted) - 1);
        self.held >>= wanted;
        self.count -= wanted;
        Ok(value)
    }
}

struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}
impl Huffman {
    fn new(lengths: &[u8]) -> Result<Self, String> {
        let mut counts = [0u16; 16];
        for &length in lengths {
            *counts
                .get_mut(usize::from(length))
                .ok_or("a deflate code is too long")? += 1;
        }
        counts[0] = 0;
        let mut offsets = [0u16; 16];
        for length in 1..15 {
            offsets[length + 1] = offsets[length] + counts[length];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                symbols[usize::from(offsets[usize::from(length)])] = symbol as u16;
                offsets[usize::from(length)] += 1;
            }
        }
        Ok(Self { counts, symbols })
    }
    fn decode(&self, reader: &mut BitReader<'_>) -> Result<u16, String> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for length in 1..16 {
            code |= reader.bits(1)? as i32;
            let count = i32::from(self.counts[length]);
            if code - count < first {
                return self
                    .symbols
                    .get((index + code - first) as usize)
                    .copied()
                    .ok_or_else(|| "a deflate code is bad".to_string());
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("a deflate code is bad".into())
    }
}

fn read_dynamic_tables(reader: &mut BitReader<'_>) -> Result<(Huffman, Huffman), String> {
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let literals = reader.bits(5)? as usize + 257;
    let distances = reader.bits(5)? as usize + 1;
    let codes = reader.bits(4)? as usize + 4;
    let mut code_lengths = [0u8; 19];
    for &slot in ORDER.iter().take(codes) {
        code_lengths[slot] = reader.bits(3)? as u8;
    }
    let table = Huffman::new(&code_lengths)?;
    let mut lengths = vec![0u8; literals + distances];
    let mut at = 0;
    while at < lengths.len() {
        let symbol = table.decode(reader)?;
        let (value, repeat) = match symbol {
            0..=15 => (symbol as u8, 1),
            16 => (
                *lengths
                    .get(at.wrapping_sub(1))
                    .ok_or("a deflate table is bad")?,
                reader.bits(2)? as usize + 3,
            ),
            17 => (0, reader.bits(3)? as usize + 3),
            _ => (0, reader.bits(7)? as usize + 11),
        };
        for _ in 0..repeat {
            *lengths.get_mut(at).ok_or("a deflate table is bad")? = value;
            at += 1;
        }
    }
    Ok((
        Huffman::new(&lengths[..literals])?,
        Huffman::new(&lengths[literals..])?,
    ))
}

fn inflate_block(
    reader: &mut BitReader<'_>,
    literal: &Huffman,
    distance: &Huffman,
    out: &mut Vec<u8>,
) -> Result<(), String> {
    loop {
        let symbol = usize::from(literal.decode(reader)?);
        match symbol {
            0..=255 => out.push(symbol as u8),
            256 => return Ok(()),
            _ => {
                let index = symbol - 257;
                let base = *LENGTH_BASE.get(index).ok_or("a deflate length is bad")?;
                let extra = u32::from(LENGTH_EXTRA[index]);
                let length = usize::from(base) + reader.bits(extra)? as usize;
                let slot = usize::from(distance.decode(reader)?);
                let base = *DISTANCE_BASE.get(slot).ok_or("a deflate distance is bad")?;
                let extra = u32::from(DISTANCE_EXTRA[slot]);
                let back = usize::from(base) + reader.bits(extra)? as usize;
                let start = out
                    .len()
                    .checked_sub(back)
                    .ok_or("a deflate distance reaches too far")?;
                for position in start..start + length {
                    out.push(out[position]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_text_and_noise_and_every_match_length() {
        let mut compressor = Compressor::default();
        let mut cases: Vec<Vec<u8>> = vec![
            Vec::new(),
            b"a".to_vec(),
            b"abcabcabcabcabc".to_vec(),
            "1 0 0 1 72 720 Tm <0001000200030004> Tj\n"
                .repeat(5000)
                .into_bytes(),
            vec![0u8; 100_000],
        ];
        // A plain counter's mix, so that this test needs no random crate and never changes.
        let mut value = 12345u32;
        cases.push(
            (0..70_000)
                .map(|_| {
                    value = value.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    (value >> 16) as u8
                })
                .collect(),
        );
        for length in 3..=260 {
            let mut data = vec![7u8; length];
            data.push(9);
            data.extend(std::iter::repeat_n(7u8, length));
            cases.push(data);
        }
        for case in &cases {
            let packed = compressor.compress(case);
            assert_eq!(&inflate(&packed).unwrap(), case);
        }
    }

    #[test]
    fn text_gets_much_smaller() {
        let text = "BT /F1 7.5 Tf 1 0 0 1 72 720 Tm [<00010002>] TJ ET\n".repeat(2000);
        let packed = Compressor::default().compress(text.as_bytes());
        assert!(packed.len() * 3 < text.len(), "{}", packed.len());
    }

    #[test]
    fn bad_input_is_an_error_and_not_a_panic() {
        for bad in [&b""[..], &b"\x78"[..], &b"\x78\x01\xff\xff\xff"[..]] {
            assert!(inflate(bad).is_err());
        }
    }
}
