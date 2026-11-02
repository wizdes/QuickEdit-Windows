//! Byte fixtures written by hand, from the published mapping of each encoding.
//! None of them comes from this crate's `encode`.
use qe_api::{EncodingChoice, TextEncoding as T};
use qe_codec::{
    CodecError, Endian, Format, LineEnding, can_encode, decode, decode_file, encode, is_available,
    transcode_to_utf8_file,
};
use std::path::PathBuf;

/// One non-ASCII character of each encoding, and its bytes.
const LEGACY: &[(T, &str, &[u8])] = &[
    (T::Windows874, "\u{0e01}", &[0xa1]),
    (T::Windows1250, "\u{0160}", &[0x8a]),
    (T::Windows1251, "\u{042f}", &[0xdf]),
    (T::Windows1252, "\u{20ac}", &[0x80]),
    (T::Windows1253, "\u{03a9}", &[0xd9]),
    (T::Windows1254, "\u{015f}", &[0xfe]),
    (T::Windows1255, "\u{05d0}", &[0xe0]),
    (T::Windows1256, "\u{0627}", &[0xc7]),
    (T::Windows1257, "\u{0101}", &[0xe2]),
    (T::Windows1258, "\u{20ab}", &[0xfe]),
    (T::ShiftJis, "\u{6f22}", &[0x8a, 0xbf]),
    (T::Gbk, "\u{6c49}", &[0xba, 0xba]),
    (T::Gb18030, "\u{1f600}", &[0x94, 0x39, 0xfc, 0x36]),
    (T::Big5, "\u{6f22}", &[0xba, 0x7e]),
    (T::Korean949, "\u{d55c}", &[0xc7, 0xd1]),
    (T::EucJp, "\u{6f22}", &[0xb4, 0xc1]),
    (T::Iso8859_1, "\u{e9}", &[0xe9]),
    (T::Iso8859_2, "\u{0160}", &[0xa9]),
    (T::Iso8859_3, "\u{0126}", &[0xa1]),
    (T::Iso8859_4, "\u{0138}", &[0xa2]),
    (T::Iso8859_5, "\u{042f}", &[0xcf]),
    (T::Iso8859_6, "\u{0627}", &[0xc7]),
    (T::Iso8859_7, "\u{03a9}", &[0xd9]),
    (T::Iso8859_8, "\u{05d0}", &[0xe0]),
    (T::Iso8859_9, "\u{015f}", &[0xfe]),
    (T::Iso8859_10, "\u{0104}", &[0xa1]),
    (T::Iso8859_11, "\u{0e01}", &[0xa1]),
    (T::Iso8859_13, "\u{0105}", &[0xe0]),
    (T::Iso8859_14, "\u{1e02}", &[0xa1]),
    (T::Iso8859_15, "\u{20ac}", &[0xa4]),
    (T::Iso8859_16, "\u{0104}", &[0xa1]),
    (T::Koi8R, "\u{042f}", &[0xf1]),
    (T::MacRoman, "\u{e9}", &[0x8e]),
    (T::MacJapanese, "\u{6f22}", &[0x8a, 0xbf]),
    (T::MacChineseTraditional, "\u{6f22}", &[0xba, 0x7e]),
    (T::MacKorean, "\u{d55c}", &[0xc7, 0xd1]),
    (T::MacChineseSimplified, "\u{6c49}", &[0xba, 0xba]),
];

fn specific(e: T) -> EncodingChoice {
    EncodingChoice::Specific(e)
}
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("qe-fix-{name}-{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
/// `transcode_to_utf8_file` on these bytes: the text and format, or the error text.
fn through_stream(bytes: &[u8], choice: EncodingChoice) -> Result<(String, Format), String> {
    let dir = scratch(&format!("stream-{:?}", std::thread::current().id()));
    let (input, output) = (dir.join("in"), dir.join("out"));
    std::fs::write(&input, bytes).unwrap();
    let result = transcode_to_utf8_file(&input, &output, choice, |_, _| {}, || false)
        .map(|format| (std::fs::read_to_string(&output).unwrap(), format))
        .map_err(|e| e.to_string());
    std::fs::remove_dir_all(dir).unwrap();
    result
}
/// Decodes through memory and stream, requires them to agree and return `text`,
/// not lossy, and saves back byte for byte.
fn assert_round_trip(bytes: &[u8], choice: EncodingChoice, text: &str, context: &str) -> Format {
    let d = decode(bytes.to_vec(), choice).unwrap_or_else(|e| panic!("{context}: {e}"));
    assert_eq!(d.text, text, "{context}: text");
    assert!(!d.format.lossy, "{context}: marked lossy");
    assert_eq!(
        encode(&d.text, &d.format).unwrap(),
        bytes,
        "{context}: save"
    );
    let (stream_text, stream_format) =
        through_stream(bytes, choice).unwrap_or_else(|e| panic!("{context}: stream: {e}"));
    assert_eq!(stream_text, text, "{context}: stream text");
    assert_eq!(stream_format, d.format, "{context}: stream format");
    let dir = scratch(&format!("whole-{:?}", std::thread::current().id()));
    let input = dir.join("in");
    std::fs::write(&input, bytes).unwrap();
    let whole = decode_file(&input, choice, |_, _| {}, || false).unwrap();
    assert_eq!(whole, d, "{context}: decode_file");
    std::fs::remove_dir_all(dir).unwrap();
    d.format
}

#[test]
fn every_legacy_encoding_reads_and_writes_its_hand_written_bytes() {
    let listed: Vec<T> = LEGACY.iter().map(|row| row.0).collect();
    let mut unavailable = Vec::new();
    for &e in T::ALL {
        let unicode = matches!(
            e,
            T::Utf8 | T::Utf16 | T::Utf16Le | T::Utf16Be | T::Utf32 | T::Utf32Le | T::Utf32Be
        );
        assert!(unicode || listed.contains(&e), "no fixture for {e:?}");
    }
    for &(e, text, bytes) in LEGACY {
        if !is_available(e) {
            unavailable.push(e.name());
            assert!(matches!(
                decode(bytes.to_vec(), specific(e)),
                Err(CodecError::UnavailableEncoding(_))
            ));
            continue;
        }
        let format = assert_round_trip(bytes, specific(e), text, e.name());
        assert_eq!(format.encoding, e);
        assert!(can_encode(text, e).unwrap(), "{}", e.name());
        assert_eq!(
            encode(text, &Format::new_document().changed_to(e)).unwrap(),
            bytes,
            "{}",
            e.name()
        );
    }
    println!("unavailable on this PC: {unavailable:?}");
    // Windows does not install these four by default. Anything else missing is a fault.
    for name in &unavailable {
        assert!(
            ["ISO-8859-10", "ISO-8859-11", "ISO-8859-14", "ISO-8859-16"].contains(name),
            "{name} is unavailable"
        );
    }
}

#[test]
fn unicode_encodings_read_and_write_hand_written_bytes_with_mixed_endings() {
    // a é 漢 😀, then CR LF, LF, CR, and LF again, so LF is the most common.
    let text = "a\u{e9}\u{6f22}\u{1f600}\r\n\n\r\n";
    let utf8: &[u8] = &[
        0x61, 0xc3, 0xa9, 0xe6, 0xbc, 0xa2, 0xf0, 0x9f, 0x98, 0x80, 0x0d, 0x0a, 0x0a, 0x0d, 0x0a,
    ];
    let units_be: &[u8] = &[
        0x00, 0x61, 0x00, 0xe9, 0x6f, 0x22, 0xd8, 0x3d, 0xde, 0x00, 0x00, 0x0d, 0x00, 0x0a, 0x00,
        0x0a, 0x00, 0x0d, 0x00, 0x0a,
    ];
    let units_le: Vec<u8> = units_be.chunks(2).flat_map(|u| [u[1], u[0]]).collect();
    let scalars_be: &[u8] = &[
        0, 0, 0, 0x61, 0, 0, 0, 0xe9, 0, 0, 0x6f, 0x22, 0, 1, 0xf6, 0, 0, 0, 0, 0x0d, 0, 0, 0,
        0x0a, 0, 0, 0, 0x0a, 0, 0, 0, 0x0d, 0, 0, 0, 0x0a,
    ];
    let scalars_le: Vec<u8> = scalars_be
        .chunks(4)
        .flat_map(|u| [u[3], u[2], u[1], u[0]])
        .collect();
    let with = |head: &[u8], body: &[u8]| [head, body].concat();
    type Case = (EncodingChoice, Vec<u8>, T, bool, Option<Endian>);
    let cases: Vec<Case> = vec![
        (specific(T::Utf8), utf8.to_vec(), T::Utf8, false, None),
        (
            specific(T::Utf8),
            with(&[0xef, 0xbb, 0xbf], utf8),
            T::Utf8,
            true,
            None,
        ),
        (
            specific(T::Utf16),
            with(&[0xff, 0xfe], &units_le),
            T::Utf16,
            true,
            Some(Endian::Little),
        ),
        (
            specific(T::Utf16),
            with(&[0xfe, 0xff], units_be),
            T::Utf16Be,
            true,
            Some(Endian::Big),
        ),
        (
            specific(T::Utf16),
            units_be.to_vec(),
            T::Utf16,
            false,
            Some(Endian::Big),
        ),
        (
            specific(T::Utf16Le),
            units_le.clone(),
            T::Utf16Le,
            false,
            Some(Endian::Little),
        ),
        (
            specific(T::Utf16Le),
            with(&[0xff, 0xfe], &units_le),
            T::Utf16Le,
            true,
            Some(Endian::Little),
        ),
        (
            specific(T::Utf16Be),
            units_be.to_vec(),
            T::Utf16Be,
            false,
            Some(Endian::Big),
        ),
        (
            specific(T::Utf16Be),
            with(&[0xfe, 0xff], units_be),
            T::Utf16Be,
            true,
            Some(Endian::Big),
        ),
        (
            specific(T::Utf32),
            with(&[0xff, 0xfe, 0, 0], &scalars_le),
            T::Utf32,
            true,
            Some(Endian::Little),
        ),
        (
            specific(T::Utf32),
            with(&[0, 0, 0xfe, 0xff], scalars_be),
            T::Utf32Be,
            true,
            Some(Endian::Big),
        ),
        (
            specific(T::Utf32),
            scalars_be.to_vec(),
            T::Utf32,
            false,
            Some(Endian::Big),
        ),
        (
            specific(T::Utf32Le),
            scalars_le.clone(),
            T::Utf32Le,
            false,
            Some(Endian::Little),
        ),
        (
            specific(T::Utf32Be),
            scalars_be.to_vec(),
            T::Utf32Be,
            false,
            Some(Endian::Big),
        ),
        (
            EncodingChoice::Automatic,
            with(&[0xfe, 0xff], units_be),
            T::Utf16Be,
            true,
            Some(Endian::Big),
        ),
        (
            EncodingChoice::Automatic,
            with(&[0, 0, 0xfe, 0xff], scalars_be),
            T::Utf32Be,
            true,
            Some(Endian::Big),
        ),
    ];
    for (choice, bytes, encoding, has_bom, endian) in cases {
        let context = format!("{choice:?} {encoding:?} bom={has_bom}");
        let format = assert_round_trip(&bytes, choice, text, &context);
        assert_eq!(format.encoding, encoding, "{context}");
        assert_eq!(format.has_bom, has_bom, "{context}");
        assert_eq!(format.endian, endian, "{context}");
        // CR LF twice, LF once and CR zero times: CR LF wins here.
        assert_eq!(format.main_line_ending, LineEnding::CrLf, "{context}");
    }
}

#[test]
fn legacy_encodings_keep_mixed_line_endings() {
    // x CR LF y LF z CR w LF: two LF, one CR LF, one CR.
    let text_of = |c: &str| format!("{c}\r\n{c}\n{c}\r{c}\n");
    for (e, c, char_bytes) in [
        (T::Windows1252, "\u{e9}", &[0xe9u8][..]),
        (T::ShiftJis, "\u{6f22}", &[0x8a, 0xbf]),
        (T::Gb18030, "\u{6c49}", &[0xba, 0xba]),
        (T::EucJp, "\u{6f22}", &[0xb4, 0xc1]),
        (T::Koi8R, "\u{042f}", &[0xf1]),
    ] {
        let mut bytes = Vec::new();
        for ending in [&b"\r\n"[..], b"\n", b"\r", b"\n"] {
            bytes.extend_from_slice(char_bytes);
            bytes.extend_from_slice(ending);
        }
        let format = assert_round_trip(&bytes, specific(e), &text_of(c), e.name());
        assert_eq!(format.main_line_ending, LineEnding::Lf, "{}", e.name());
    }
}

#[test]
fn main_line_ending_ties_follow_the_mac() {
    for (bytes, main) in [
        (&b"a\nb\r\n"[..], LineEnding::Lf),
        (b"a\r\nb\r", LineEnding::CrLf),
        (b"a\r\nb\r\nc\n", LineEnding::CrLf),
        (b"a\rb\r\nc\r", LineEnding::Cr),
        (b"plain", LineEnding::Lf),
    ] {
        let d = decode(bytes.to_vec(), EncodingChoice::Automatic).unwrap();
        assert_eq!(d.format.main_line_ending, main, "{bytes:?}");
    }
}

/// Item 1: valid UTF-8 stays valid wherever a character falls on a 1 MiB piece end.
#[test]
fn valid_utf8_over_one_mib_opens_as_utf8_at_every_offset() {
    let whole = "\u{6f22}".repeat(400_000);
    assert_round_trip(
        whole.as_bytes(),
        EncodingChoice::Automatic,
        &whole,
        "400,000 of U+6F22",
    );
    const MIB: usize = 1024 * 1024;
    for character in ["\u{e9}", "\u{6f22}", "\u{1f600}"] {
        for padding in 0..character.len() + 2 {
            // The characters straddle the first and later piece ends in every position.
            for run in [2usize, 5] {
                let mut text = "a".repeat(MIB - padding);
                text.push_str(&character.repeat(run));
                text.push_str(&"b".repeat(MIB - 1));
                text.push_str(&character.repeat(run));
                let context = format!("{character:?} padding {padding} run {run}");
                let format =
                    assert_round_trip(text.as_bytes(), EncodingChoice::Automatic, &text, &context);
                assert_eq!(format.encoding, T::Utf8, "{context}");
                assert_round_trip(text.as_bytes(), specific(T::Utf8), &text, &context);
            }
        }
    }
    // A bad byte after the piece end, and a cut character at the very end, still fail.
    let mut bad = "a".repeat(MIB - 1).into_bytes();
    bad.extend([0xe6, 0xbc, 0x41]);
    assert!(matches!(
        decode(bad.clone(), specific(T::Utf8)),
        Err(CodecError::InapplicableEncoding(T::Utf8))
    ));
    assert!(through_stream(&bad, specific(T::Utf8)).is_err());
    let mut cut = "a".repeat(MIB + 10).into_bytes();
    cut.extend([0xf0, 0x9f]);
    assert!(decode(cut.clone(), specific(T::Utf8)).is_err());
    assert!(through_stream(&cut, specific(T::Utf8)).is_err());
}

/// Item 2.
#[test]
fn big_endian_bom_with_a_chosen_unicode_encoding_saves_byte_for_byte() {
    let d = decode(vec![0xfe, 0xff, 0x00, 0x61], specific(T::Utf16)).unwrap();
    assert_eq!(d.text, "a");
    assert!(!d.format.lossy);
    assert_eq!(d.format.endian, Some(Endian::Big));
    assert_eq!(
        encode(&d.text, &d.format).unwrap(),
        [0xfe, 0xff, 0x00, 0x61]
    );
    let bytes = [0, 0, 0xfe, 0xff, 0, 0, 0, 0x61];
    let d = decode(bytes.to_vec(), specific(T::Utf32)).unwrap();
    assert_eq!(d.text, "a");
    assert!(!d.format.lossy);
    assert_eq!(encode(&d.text, &d.format).unwrap(), bytes);
    assert_round_trip(&bytes, specific(T::Utf32), "a", "stream UTF-32");
    assert_round_trip(&[0xfe, 0xff, 0, 0x61], specific(T::Utf16), "a", "UTF-16");
}

/// Item 3. Bytes that are not valid in each page.
#[test]
fn a_chosen_legacy_encoding_refuses_bad_bytes() {
    for (e, bytes) in [
        (T::ShiftJis, &[0x81u8, 0x20][..]),
        (T::Gbk, &[0x81, 0x20]),
        (T::Korean949, &[0x81, 0x20]),
        (T::Big5, &[0x81, 0x20]),
        (T::EucJp, &[0xa1, 0x20]),
        (T::Iso8859_3, &[0xa5]),
        (T::Windows1253, &[0xaa]),
        (T::Gb18030, &[0x81, 0x20]),
        // A lead byte with nothing after it.
        (T::ShiftJis, &[b'a', 0x8a]),
    ] {
        let name = e.name();
        match decode(bytes.to_vec(), specific(e)) {
            Err(CodecError::InapplicableEncoding(named)) => assert_eq!(named, e),
            other => panic!("{name}: {other:?}"),
        }
        assert_eq!(
            through_stream(bytes, specific(e)).unwrap_err(),
            format!("could not be opened using text encoding {name}"),
        );
    }
}

/// A character that reads but writes back as other bytes stays lossy, not an error.
#[test]
fn duplicate_mappings_in_a_chosen_encoding_are_lossy() {
    let d = decode(vec![0xee, 0xef], specific(T::ShiftJis)).unwrap();
    assert_eq!(d.text, "\u{2170}");
    assert!(d.format.lossy);
    assert_eq!(encode(&d.text, &d.format).unwrap(), [0xfa, 0x40]);
    let stream = through_stream(&[0xee, 0xef], specific(T::ShiftJis)).unwrap();
    assert!(stream.1.lossy);
}

/// Item 15: the Automatic path on this PC's real system code page.
#[test]
fn automatic_opens_every_byte_value() {
    let bytes: Vec<u8> = (0..=255).collect();
    let d = decode(bytes.clone(), EncodingChoice::Automatic).unwrap();
    let (text, format) = through_stream(&bytes, EncodingChoice::Automatic).unwrap();
    assert_eq!(format, d.format);
    assert_eq!(text, d.text);
    println!("system page read this as {:?}", d.format.encoding);
    // Every ASCII byte reads as itself on every Windows system page, and a byte
    // over 7F always gives some text. `src/text.rs` checks each byte by hand.
    let ascii: String = (0u8..0x80).map(char::from).collect();
    assert!(d.text.starts_with(&ascii), "{:?}", d.text);
    assert!(d.text.len() > ascii.len());
    if d.format.encoding == T::Utf8 {
        assert!(d.format.lossy);
        assert_eq!(d.text, String::from_utf8_lossy(&bytes));
    }
}

/// Items 14 and 17: the same fault gives the same text from both paths.
#[test]
fn memory_and_stream_report_the_same_error_text() {
    for (bytes, choice, wanted) in [
        (
            vec![0, 0, 0xfe, 0xff, 0, 0x11, 0, 0],
            EncodingChoice::Automatic,
            "UTF-32",
        ),
        (
            vec![0xff, 0xfe, 0, 0, 0, 0, 0x11, 0],
            EncodingChoice::Automatic,
            "UTF-32",
        ),
        (
            vec![0, 0, 0xfe, 0xff, 0, 0x11, 0, 0],
            specific(T::Utf32),
            "UTF-32",
        ),
        (vec![0, 0x11, 0, 0], specific(T::Utf32Be), "UTF-32 BE"),
        (vec![0, 0, 0x11, 0], specific(T::Utf32Le), "UTF-32 LE"),
        (vec![0xff], specific(T::Utf8), "UTF-8"),
        (vec![0xaa], specific(T::Windows1253), "Windows-1253"),
    ] {
        let memory = decode(bytes.clone(), choice).unwrap_err().to_string();
        let stream = through_stream(&bytes, choice).unwrap_err();
        assert_eq!(memory, stream, "{bytes:?}");
        assert_eq!(
            memory,
            format!("could not be opened using text encoding {wanted}")
        );
    }
}

/// Item 17: the scan gives progress and can be cancelled, before any output exists.
#[test]
fn cancel_and_progress_work_during_the_utf8_scan() {
    let dir = scratch("scan");
    let (input, output) = (dir.join("in"), dir.join("out"));
    const SIZE: u64 = 5 * 1024 * 1024;
    std::fs::write(&input, vec![b'a'; SIZE as usize]).unwrap();
    let mut seen = Vec::new();
    let mut asked = 0;
    let result = transcode_to_utf8_file(
        &input,
        &output,
        EncodingChoice::Automatic,
        |done, total| seen.push((done, total)),
        || {
            asked += 1;
            asked == 3
        },
    );
    assert!(matches!(result, Err(CodecError::Cancelled)));
    assert!(!output.exists(), "the scan made an output file");
    assert_eq!(seen, [(1 << 20, SIZE * 2), (2 << 20, SIZE * 2)]);

    let mut seen = Vec::new();
    transcode_to_utf8_file(
        &input,
        &output,
        EncodingChoice::Automatic,
        |done, total| seen.push((done, total)),
        || false,
    )
    .unwrap();
    assert!(seen.is_sorted(), "progress fell: {seen:?}");
    assert_eq!(seen.last(), Some(&(SIZE * 2, SIZE * 2)));
    assert!(seen.len() >= 10, "{seen:?}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn multibyte_characters_cross_piece_ends_in_every_multibyte_encoding() {
    const MIB: usize = 1024 * 1024;
    for &(e, text, bytes) in LEGACY {
        if bytes.len() < 2 || !is_available(e) {
            continue;
        }
        for shift in 0..bytes.len() {
            let mut file = vec![b'a'; shift + 1];
            let mut expected = "a".repeat(shift + 1);
            while file.len() < 2 * MIB + 16 {
                file.extend_from_slice(bytes);
                expected.push_str(text);
            }
            let context = format!("{} shift {shift}", e.name());
            assert_round_trip(&file, specific(e), &expected, &context);
        }
    }
}

#[test]
fn utf16_pairs_and_utf32_units_cross_piece_ends() {
    const MIB: usize = 1024 * 1024;
    for pad in [0usize, 1] {
        let mut file = vec![0xfe, 0xff];
        let mut expected = String::new();
        for _ in 0..pad {
            file.extend([0, b'a']);
            expected.push('a');
        }
        while file.len() < 2 * MIB + 8 {
            file.extend([0xd8, 0x3d, 0xde, 0x00]);
            expected.push('\u{1f600}');
        }
        assert_round_trip(&file, EncodingChoice::Automatic, &expected, "UTF-16 pairs");
    }
    let mut file = vec![0xff, 0xfe, 0, 0];
    let mut expected = String::new();
    while file.len() < 2 * MIB + 8 {
        file.extend([0x00, 0xf6, 0x01, 0x00]);
        expected.push('\u{1f600}');
    }
    assert_round_trip(&file, EncodingChoice::Automatic, &expected, "UTF-32");
}

/// The decided EUC-JP limit. U+4E02 is in JIS X 0212 (standard form 8F B0 A1).
#[test]
fn euc_jp_refuses_what_code_page_20932_cannot_write_in_standard_form() {
    assert!(can_encode("\u{6f22}\u{ff71}a", T::EucJp).unwrap());
    let format = Format::new_document().changed_to(T::EucJp);
    assert_eq!(
        encode("\u{6f22}\u{ff71}a", &format).unwrap(),
        [0xb4, 0xc1, 0x8e, 0xb1, 0x61]
    );
    assert!(!can_encode("\u{4e02}", T::EucJp).unwrap());
    assert!(matches!(
        encode("\u{4e02}", &format),
        Err(CodecError::Unencodable(T::EucJp))
    ));
}

/// Item 4: standard JIS X 0212 reads as the right character, or not at all. Values
/// are from the published JIS X 0212 table. Writing stays refused, so the file is
/// marked lossy and a save in EUC-JP gives an error, not other bytes.
#[test]
fn euc_jp_reads_standard_jis_x_0212() {
    for (bytes, text) in [
        (&[0x8f, 0xb0, 0xa1][..], "\u{4e02}"),
        (&[0x8f, 0xb0, 0xa2], "\u{4e04}"),
        (
            &[b'a', 0x8f, 0xb0, 0xa1, 0xb4, 0xc1, 0x8e, 0xb1],
            "a\u{4e02}\u{6f22}\u{ff71}",
        ),
    ] {
        let d = decode(bytes.to_vec(), specific(T::EucJp)).unwrap();
        assert_eq!(d.text, text, "{bytes:02x?}");
        assert!(d.format.lossy, "this crate cannot write it back");
        assert!(matches!(
            encode(&d.text, &d.format),
            Err(CodecError::Unencodable(T::EucJp))
        ));
        let (stream, format) = through_stream(bytes, specific(T::EucJp)).unwrap();
        assert_eq!(stream, text);
        assert_eq!(format, d.format);
    }
    // These show the wrong letters, or none, if read by code page 20932 alone.
    for bytes in [
        &[0x8f, 0xb0][..],
        &[0x8f, 0x21, 0x21],
        &[0x8f, 0xa1, 0x21],
        &[0x80],
        &[0xb0, 0x21],
        &[0xff],
        &[0x8e, 0x21],
    ] {
        match decode(bytes.to_vec(), specific(T::EucJp)) {
            Err(CodecError::InapplicableEncoding(T::EucJp)) => {}
            other => panic!("{bytes:02x?}: {other:?}"),
        }
        assert_eq!(
            through_stream(bytes, specific(T::EucJp)).unwrap_err(),
            "could not be opened using text encoding EUC-JP",
            "{bytes:02x?}"
        );
    }
    // A three-byte character across a piece end.
    let mut file = vec![b'a'; (1 << 20) - 1];
    file.extend([0x8f, 0xb0, 0xa1]);
    let (stream, _) = through_stream(&file, specific(T::EucJp)).unwrap();
    assert!(stream.ends_with('\u{4e02}'));
    assert_eq!(stream.len(), (1 << 20) - 1 + 3);
}

/// Item 8: an `endian` on a page that has no byte order must not change the bytes.
#[test]
fn encode_refuses_a_format_whose_fields_disagree() {
    let check = |format: &Format| {
        assert!(
            matches!(encode("a", format), Err(CodecError::InvalidFormat { .. })),
            "{format:?}"
        );
    };
    let mut bad = Format::new_document().changed_to(T::Windows1252);
    bad.endian = Some(Endian::Little);
    check(&bad);
    let mut bad = Format::new_document();
    bad.endian = Some(Endian::Big);
    check(&bad);
    let mut bad = Format::new_document().changed_to(T::Windows1252);
    bad.has_bom = true;
    check(&bad);
    let mut bad = Format::new_document().changed_to(T::Utf16Le);
    bad.endian = Some(Endian::Big);
    check(&bad);
    let mut bad = Format::new_document().changed_to(T::Utf32Be);
    bad.endian = Some(Endian::Little);
    check(&bad);

    let new = |e, bom, endian| Format::new(e, bom, endian, LineEnding::Lf, false);
    assert!(matches!(
        new(T::Windows1252, false, Some(Endian::Little)),
        Err(CodecError::InvalidFormat { .. })
    ));
    assert!(matches!(
        new(T::ShiftJis, true, None),
        Err(CodecError::InvalidFormat { .. })
    ));
    assert!(matches!(
        new(T::Utf16Be, false, Some(Endian::Little)),
        Err(CodecError::InvalidFormat { .. })
    ));
    for good in [
        new(T::Utf16, true, Some(Endian::Little)),
        new(T::Utf16, false, Some(Endian::Big)),
        new(T::Utf16Be, true, Some(Endian::Big)),
        new(T::Utf8, true, None),
        new(T::Windows1252, false, None),
    ] {
        encode("a", &good.unwrap()).unwrap();
    }
}

/// Item 11: done never passes total, and an empty file reports (0, 0) as its finish.
#[test]
fn an_empty_file_reports_zero_of_zero_once_from_the_streaming_calls() {
    let dir = scratch("empty");
    let (input, output) = (dir.join("in"), dir.join("out"));
    std::fs::write(&input, b"").unwrap();
    for choice in [EncodingChoice::Automatic, specific(T::Windows1252)] {
        let mut seen = Vec::new();
        transcode_to_utf8_file(&input, &output, choice, |d, t| seen.push((d, t)), || false)
            .unwrap();
        assert_eq!(seen, [(0, 0)], "{choice:?}");
        std::fs::remove_file(&output).unwrap();
        let mut seen = Vec::new();
        let d = decode_file(&input, choice, |d, t| seen.push((d, t)), || false).unwrap();
        assert_eq!(d.text, "");
        assert_eq!(seen, [(0, 0)], "{choice:?}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// Item 5: one pass, one file size in `total`, for every route.
#[test]
fn decode_file_reports_one_file_size_and_rises_to_it() {
    let dir = scratch("one-pass");
    let input = dir.join("in");
    const SIZE: usize = 3 << 20;
    for (bytes, choice) in [
        (vec![b'a'; SIZE], EncodingChoice::Automatic),
        (vec![b'a'; SIZE], specific(T::Utf8)),
        (vec![0xe9; SIZE], specific(T::Windows1252)),
    ] {
        std::fs::write(&input, &bytes).unwrap();
        let mut seen = Vec::new();
        decode_file(&input, choice, |d, t| seen.push((d, t)), || false).unwrap();
        assert!(seen.is_sorted(), "{seen:?}");
        assert!(
            seen.iter().all(|(d, t)| *t == SIZE as u64 && d <= t),
            "{seen:?}"
        );
        assert_eq!(seen.last(), Some(&(SIZE as u64, SIZE as u64)));
        assert!(seen.len() >= 3, "{seen:?}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// Item 5: a file that is like its first 1 MiB is read once. `cancel` is asked once
/// for every 1 MiB read, so more asks would mean another pass.
#[test]
fn decode_file_reads_a_uniform_file_once() {
    let dir = scratch("once");
    let input = dir.join("in");
    const MIB: usize = 1 << 20;
    for (bytes, choice) in [
        (vec![b'a'; 8 * MIB], EncodingChoice::Automatic),
        (vec![0xe9; 8 * MIB], specific(T::Windows1252)),
        ([0x8a, 0xbf].repeat(4 * MIB), specific(T::ShiftJis)),
        (b"ab".repeat(4 * MIB), specific(T::Utf16Le)),
    ] {
        std::fs::write(&input, &bytes).unwrap();
        let mut asked = 0;
        decode_file(
            &input,
            choice,
            |_, _| {},
            || {
                asked += 1;
                false
            },
        )
        .unwrap();
        assert!((8..=10).contains(&asked), "{choice:?}: {asked} asks");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// Item 5: text that grows past the first estimate still comes out whole, with one
/// allocation, for each shape of file. The result must match the in-memory route.
#[test]
fn decode_file_handles_text_that_outgrows_or_undershoots_the_first_estimate() {
    let dir = scratch("uneven");
    let input = dir.join("in");
    let ascii = vec![b'a'; 3 << 20];
    let kanji = [0x8au8, 0xbf].repeat(3 << 19);
    for (name, first, second) in [
        ("ASCII, then kanji", &ascii, &kanji),
        ("kanji, then ASCII", &kanji, &ascii),
    ] {
        let mut file = first.clone();
        file.extend_from_slice(second);
        std::fs::write(&input, &file).unwrap();
        let memory = decode(file, specific(T::ShiftJis)).unwrap();
        let mut last = (0, 0);
        let mut falls = 0;
        let streamed = decode_file(
            &input,
            specific(T::ShiftJis),
            |d, t| {
                falls += u32::from(d < last.0);
                last = (d, t);
            },
            || false,
        )
        .unwrap();
        assert_eq!(streamed, memory, "{name}");
        assert_eq!(falls, 0, "{name}");
        assert_eq!(last.0, last.1, "{name}");
        // The string must not hold on to a much larger block than it needs.
        assert!(
            streamed.text.capacity() <= streamed.text.len() + streamed.text.len() / 4 + (2 << 20),
            "{name}: {} of {}",
            streamed.text.len(),
            streamed.text.capacity()
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// Automatic, for a file whose bad byte comes after the first estimate.
#[test]
fn decode_file_falls_back_after_a_late_bad_byte() {
    let dir = scratch("late");
    let input = dir.join("in");
    let mut file = vec![b'a'; 3 << 20];
    file.push(0xff);
    file.extend_from_slice(b"\r\n");
    std::fs::write(&input, &file).unwrap();
    let memory = decode(file, EncodingChoice::Automatic).unwrap();
    let d = decode_file(&input, EncodingChoice::Automatic, |_, _| {}, || false).unwrap();
    assert_eq!(d, memory);
    std::fs::remove_dir_all(dir).unwrap();
}

/// Runs `decode_file` while `change` runs at the first progress report.
fn decode_while_changing(
    dir: &std::path::Path,
    start: &[u8],
    choice: EncodingChoice,
    change: impl FnOnce(&std::path::Path),
) -> Result<qe_codec::Decoded, CodecError> {
    let input = dir.join("in");
    std::fs::write(&input, start).unwrap();
    let mut change = Some(change);
    let mut over = false;
    let result = decode_file(
        &input,
        choice,
        |done, total| {
            over |= done > total;
            if let Some(change) = change.take() {
                change(&input);
            }
        },
        || false,
    );
    assert!(!over, "done passed total");
    assert!(change.is_none(), "the file was never changed");
    result
}
/// Waits out one step of the file time, which moves about every 16 ms. A write of the
/// same size inside one step looks like no change at all. The README lists this limit.
fn next_time_step() {
    std::thread::sleep(std::time::Duration::from_millis(40));
}
fn append(path: &std::path::Path, bytes: &[u8]) {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

/// Item 7: every kind of change gives the one clear error.
#[test]
fn decode_file_refuses_a_file_that_changes_while_it_is_read() {
    let dir = scratch("changing");
    let mib = 1usize << 20;
    let text = vec![b'a'; 3 * mib];
    let expect = |result: Result<qe_codec::Decoded, CodecError>, what: &str| match result {
        Err(CodecError::SourceChanged { path, leftover }) => {
            assert!(path.ends_with("in"), "{what}: {path:?}");
            assert_eq!(leftover, None, "{what}");
        }
        other => panic!("{what}: {:?}", other.map(|d| d.text.len())),
    };
    for choice in [
        EncodingChoice::Automatic,
        specific(T::Windows1252),
        specific(T::Utf8),
    ] {
        expect(
            decode_while_changing(&dir, &text, choice, |p| append(p, &vec![b'b'; 27 * mib])),
            "grew",
        );
        expect(
            decode_while_changing(&dir, &text, choice, |p| {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(p)
                    .unwrap()
                    .set_len(mib as u64 / 2)
                    .unwrap();
            }),
            "shrank",
        );
        expect(
            decode_while_changing(&dir, &text, choice, |p| {
                next_time_step();
                std::fs::write(p, vec![b'c'; 3 * mib]).unwrap();
            }),
            "same size, new bytes",
        );
        expect(
            decode_while_changing(&dir, &text, choice, |p| {
                std::fs::rename(p, p.with_extension("old")).unwrap();
                std::fs::write(p, vec![b'c'; 3 * mib]).unwrap();
            }),
            "replaced",
        );
    }
    // A bad byte written in place. Automatic must not fall back or fail with the
    // encoding error: the file is not the one it started to read.
    let bad_byte = |p: &std::path::Path| {
        use std::io::{Seek, SeekFrom, Write};
        next_time_step();
        let mut file = std::fs::OpenOptions::new().write(true).open(p).unwrap();
        file.seek(SeekFrom::Start(2 * mib as u64)).unwrap();
        file.write_all(&[0xff]).unwrap();
    };
    expect(
        decode_while_changing(&dir, &text, EncodingChoice::Automatic, bad_byte),
        "bad byte added",
    );
    expect(
        decode_while_changing(&dir, &text, specific(T::Utf8), bad_byte),
        "bad byte added, chosen encoding",
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// Item 7: a file that grows stops the read at once, so it cannot use up memory.
#[test]
fn decode_file_stops_at_the_first_byte_past_the_size() {
    let dir = scratch("stops");
    let input = dir.join("in");
    let mib = 1usize << 20;
    for choice in [specific(T::Utf8), specific(T::Windows1252)] {
        std::fs::write(&input, vec![b'a'; 3 * mib]).unwrap();
        let mut asked = 0;
        let mut grown = false;
        let result = decode_file(
            &input,
            choice,
            |_, _| {
                if !grown {
                    grown = true;
                    append(&input, &vec![b'b'; 27 * mib]);
                }
            },
            || {
                asked += 1;
                false
            },
        );
        assert!(
            matches!(result, Err(CodecError::SourceChanged { .. })),
            "{choice:?}"
        );
        assert!(asked <= 5, "{choice:?}: read on for {asked} pieces");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// Item 7 applies to the streaming file route too, because both read the same way.
#[test]
fn transcode_refuses_a_file_that_changes_and_removes_its_output() {
    let dir = scratch("transcode-changing");
    let (input, output) = (dir.join("in"), dir.join("out"));
    let mib = 1usize << 20;
    std::fs::write(&input, vec![b'a'; 3 * mib]).unwrap();
    let mut grown = false;
    let result = transcode_to_utf8_file(
        &input,
        &output,
        specific(T::Windows1252),
        |done, total| {
            assert!(done <= total);
            if !grown {
                grown = true;
                append(&input, &vec![b'b'; 5 * mib]);
            }
        },
        || false,
    );
    assert!(
        matches!(result, Err(CodecError::SourceChanged { .. })),
        "{result:?}"
    );
    assert!(!output.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

/// Item 10: the promise that output is gone after a failure must not be false.
#[test]
fn transcode_names_an_output_it_could_not_delete() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = scratch("stuck-output");
    let (input, output) = (dir.join("in"), dir.join("out"));
    std::fs::write(&input, vec![b'a'; 3 << 20]).unwrap();
    let holder = std::cell::RefCell::new(None);
    let result = transcode_to_utf8_file(
        &input,
        &output,
        specific(T::Windows1252),
        |_, _| {
            *holder.borrow_mut() = Some(
                std::fs::OpenOptions::new()
                    .read(true)
                    .share_mode(3)
                    .open(&output)
                    .unwrap(),
            );
        },
        || holder.borrow().is_some(),
    );
    match result {
        Err(CodecError::CleanupFailed { cause, leftover }) => {
            assert!(matches!(*cause, CodecError::Cancelled), "{cause:?}");
            assert_eq!(leftover, output);
            assert!(output.exists());
        }
        other => panic!("{other:?}"),
    }
    drop(holder);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn can_encode_answers_for_each_kind_of_encoding() {
    assert!(can_encode("\u{1f600}", T::Utf16).unwrap());
    assert!(can_encode("\u{1f600}", T::Gb18030).unwrap());
    assert!(!can_encode("\u{1f600}", T::ShiftJis).unwrap());
    assert!(!can_encode("\u{e9}", T::Windows1251).unwrap());
    assert!(can_encode("", T::ShiftJis).unwrap());
}

#[test]
fn encode_reserves_only_what_the_output_needs() {
    let text = "a".repeat(10_000);
    for (e, size) in [(T::Utf16Le, 20_000), (T::Utf32Be, 40_000)] {
        let bytes = encode(&text, &Format::new_document().changed_to(e)).unwrap();
        assert_eq!(bytes.len(), size);
        assert!(bytes.capacity() <= size + 4, "{}", bytes.capacity());
    }
}

/// Item 14, for real: output past 2 GiB and input past 2 GiB. This needs about
/// 7 GB of memory and a minute, so run it with `-- --ignored`.
#[test]
#[ignore = "needs about 7 GB of memory"]
fn output_and_input_over_2_gib_do_not_panic() {
    // U+0080 is two bytes in UTF-8 and four in GB 18030.
    let text = "\u{80}".repeat(600_000_000);
    assert!(can_encode(&text, T::Gb18030).unwrap());
    assert!(!can_encode(&text, T::Windows1251).unwrap());
    let format = Format::new_document().changed_to(T::Gb18030);
    let bytes = encode(&text, &format).unwrap();
    assert_eq!(bytes.len(), 2_400_000_000);
    assert_eq!(&bytes[..4], [0x81, 0x30, 0x81, 0x30]);
    drop(text);
    let d = decode(bytes, specific(T::Gb18030)).unwrap();
    assert!(!d.format.lossy);
    assert_eq!(d.text.len(), 1_200_000_000);
}
