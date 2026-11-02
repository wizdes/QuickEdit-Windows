//! A PNG writer and reader for the saved images. It uses Deflate's fixed codes and
//! one kind of match, a repeat of the pixel before. Flat controls shrink a lot, with
//! no new dependency. The reader reads only what this writer writes.
use qe_api::Bitmap;

fn crc(bytes: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in bytes {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = (c >> 1) ^ if c & 1 != 0 { 0xedb8_8320 } else { 0 };
        }
    }
    !c
}
fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let check = crc(&out[start..]);
    out.extend_from_slice(&check.to_be_bytes());
}

#[derive(Default)]
struct Bits {
    out: Vec<u8>,
    held: u32,
    count: u32,
}
impl Bits {
    /// Extra bits go first-bit-low.
    fn low(&mut self, value: u32, width: u32) {
        self.held |= value << self.count;
        self.count += width;
        while self.count >= 8 {
            self.out.push((self.held & 0xff) as u8);
            self.held >>= 8;
            self.count -= 8;
        }
    }
    /// Huffman codes go first-bit-high.
    fn high(&mut self, code: u32, width: u32) {
        for bit in (0..width).rev() {
            self.low((code >> bit) & 1, 1);
        }
    }
    fn symbol(&mut self, symbol: u32) {
        match symbol {
            0..=143 => self.high(0x30 + symbol, 8),
            144..=255 => self.high(0x190 + symbol - 144, 9),
            256..=279 => self.high(symbol - 256, 7),
            _ => self.high(0xc0 + symbol - 280, 8),
        }
    }
    fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            self.out.push((self.held & 0xff) as u8);
        }
        self.out
    }
}
const LENGTHS: [(u32, u32, u32); 29] = [
    (257, 3, 0),
    (258, 4, 0),
    (259, 5, 0),
    (260, 6, 0),
    (261, 7, 0),
    (262, 8, 0),
    (263, 9, 0),
    (264, 10, 0),
    (265, 11, 1),
    (266, 13, 1),
    (267, 15, 1),
    (268, 17, 1),
    (269, 19, 2),
    (270, 23, 2),
    (271, 27, 2),
    (272, 31, 2),
    (273, 35, 3),
    (274, 43, 3),
    (275, 51, 3),
    (276, 59, 3),
    (277, 67, 4),
    (278, 83, 4),
    (279, 99, 4),
    (280, 115, 4),
    (281, 131, 5),
    (282, 163, 5),
    (283, 195, 5),
    (284, 227, 5),
    (285, 258, 0),
];
const PIXEL: usize = 4;
fn deflate(raw: &[u8]) -> Vec<u8> {
    let mut bits = Bits::default();
    bits.low(1, 1);
    bits.low(1, 2);
    let mut i = 0;
    while i < raw.len() {
        let mut run = 0;
        if i >= PIXEL {
            while i + run < raw.len() && run < 258 && raw[i + run] == raw[i + run - PIXEL] {
                run += 1;
            }
        }
        if run >= 3 {
            let (code, base, extra) = *LENGTHS
                .iter()
                .rev()
                .find(|(_, base, _)| *base as usize <= run)
                .expect("a run of 3 or more has a code");
            bits.symbol(code);
            bits.low(run as u32 - base, extra);
            // Distance code 3 means 4 bytes back, with no extra bits.
            bits.high(3, 5);
            i += run;
        } else {
            bits.symbol(u32::from(raw[i]));
            i += 1;
        }
    }
    bits.symbol(256);
    bits.finish()
}
fn adler(raw: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for byte in raw {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}
pub fn encode(image: &Bitmap) -> Vec<u8> {
    let (w, h) = (image.width as usize, image.height as usize);
    let mut raw = Vec::with_capacity((w * 4 + 1) * h);
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            let at = (y * w + x) * 4;
            let p = &image.bgra[at..at + 4];
            raw.extend_from_slice(&[p[2], p[1], p[0], 255]);
        }
    }
    let mut z = vec![0x78, 0x01];
    z.extend_from_slice(&deflate(&raw));
    z.extend_from_slice(&adler(&raw).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&image.width.to_be_bytes());
    header.extend_from_slice(&image.height.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    bit: u32,
}
impl Reader<'_> {
    fn one(&mut self) -> Result<u32, String> {
        let byte = *self
            .bytes
            .get(self.at)
            .ok_or("the image data ended early")?;
        let value = (u32::from(byte) >> self.bit) & 1;
        self.bit += 1;
        if self.bit == 8 {
            self.bit = 0;
            self.at += 1;
        }
        Ok(value)
    }
    fn low(&mut self, width: u32) -> Result<u32, String> {
        let mut value = 0;
        for i in 0..width {
            value |= self.one()? << i;
        }
        Ok(value)
    }
    fn high(&mut self, width: u32) -> Result<u32, String> {
        let mut value = 0;
        for _ in 0..width {
            value = (value << 1) | self.one()?;
        }
        Ok(value)
    }
    fn symbol(&mut self) -> Result<u32, String> {
        let seven = self.high(7)?;
        if seven <= 0x17 {
            return Ok(seven + 256);
        }
        let eight = (seven << 1) | self.one()?;
        if (0x30..=0xbf).contains(&eight) {
            return Ok(eight - 0x30);
        }
        if (0xc0..=0xc7).contains(&eight) {
            return Ok(eight - 0xc0 + 280);
        }
        let nine = (eight << 1) | self.one()?;
        Ok(nine - 0x190 + 144)
    }
}
/// The pixels of an image from `encode`, as RGBA rows with their filter bytes.
pub fn decode(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("bad PNG signature".into());
    }
    let (mut at, mut z, mut size) = (8, Vec::new(), (0, 0));
    while at + 12 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().map_err(|_| "bad length")?);
        let len = len as usize;
        let kind = &bytes[at + 4..at + 8];
        let data = bytes
            .get(at + 8..at + 8 + len)
            .ok_or("a PNG chunk ended early")?;
        if kind == b"IHDR" {
            size = (
                u32::from_be_bytes(data[0..4].try_into().map_err(|_| "bad width")?),
                u32::from_be_bytes(data[4..8].try_into().map_err(|_| "bad height")?),
            );
        } else if kind == b"IDAT" {
            z.extend_from_slice(data);
        }
        at += len + 12;
    }
    let mut r = Reader {
        bytes: &z,
        at: 2,
        bit: 0,
    };
    if r.low(3)? != 0b011 {
        return Err("this reader takes only images from its own writer".into());
    }
    let mut raw = Vec::new();
    loop {
        let symbol = r.symbol()?;
        match symbol {
            0..=255 => raw.push(symbol as u8),
            256 => break,
            _ => {
                let (_, base, extra) = *LENGTHS
                    .iter()
                    .find(|(code, _, _)| *code == symbol)
                    .ok_or("bad length code")?;
                let run = base + r.low(extra)?;
                if r.high(5)? != 3 {
                    return Err("this reader takes only a distance of 4".into());
                }
                for _ in 0..run {
                    raw.push(raw[raw.len() - PIXEL]);
                }
            }
        }
    }
    Ok((size.0, size.1, raw))
}
/// The same image as `decode` gives, for comparing.
pub fn raw_rows(image: &Bitmap) -> Vec<u8> {
    let (w, h) = (image.width as usize, image.height as usize);
    let mut raw = Vec::with_capacity((w * 4 + 1) * h);
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            let at = (y * w + x) * 4;
            let p = &image.bgra[at..at + 4];
            raw.extend_from_slice(&[p[2], p[1], p[0], 255]);
        }
    }
    raw
}
