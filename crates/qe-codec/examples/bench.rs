use qe_api::{EncodingChoice, TextEncoding};
use qe_codec::{Format, decode, encode};
use std::time::Instant;

fn main() {
    let text = "The quick brown fox jumps over the lazy dog.\r\n".repeat(2_184_534);
    println!("Input text: {} bytes", text.len());
    for encoding in [
        TextEncoding::Utf8,
        TextEncoding::Utf16,
        TextEncoding::Windows1252,
    ] {
        let format = Format::new_document().changed_to(encoding);
        let bytes = encode(&text, &format).expect("prepare fixture");
        let start = Instant::now();
        let decoded = decode(bytes.clone(), EncodingChoice::Specific(encoding)).expect("decode");
        let decode_s = start.elapsed().as_secs_f64();
        let start = Instant::now();
        let encoded = encode(&decoded.text, &decoded.format).expect("encode");
        let encode_s = start.elapsed().as_secs_f64();
        assert_eq!(bytes, encoded);
        println!(
            "{}: {} byte file; decode {:.1} MB/s ({:.3} s); encode {:.1} MB/s ({:.3} s)",
            encoding.name(),
            bytes.len(),
            bytes.len() as f64 / 1_000_000.0 / decode_s,
            decode_s,
            bytes.len() as f64 / 1_000_000.0 / encode_s,
            encode_s
        );
    }
}
