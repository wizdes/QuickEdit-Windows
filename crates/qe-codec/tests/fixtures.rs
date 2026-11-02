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
