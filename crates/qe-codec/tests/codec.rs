use qe_api::{EncodingChoice, TextEncoding};
use qe_codec::{
    AtomicWriteResult, CodecError, Endian, Format, LineEnding, atomic_write, can_encode, decode,
    encode,
};

#[test]
fn every_available_encoding_round_trips() {
    let mut unavailable = Vec::new();
    let mut count = 0;
    for &encoding in TextEncoding::ALL {
        if !qe_codec::is_available(encoding) {
            unavailable.push(encoding.name());
            continue;
        }
        let format = Format::new_document().changed_to(encoding);
        let sample = if can_encode("é", encoding).unwrap() {
            "Aé\r\nB"
        } else {
            "A\r\nB"
        };
        let bytes = encode(sample, &format).unwrap();
        let decoded = decode(bytes.clone(), EncodingChoice::Specific(encoding)).unwrap();
        assert_eq!(decoded.text, sample, "{}", encoding.name());
        assert_eq!(
            encode(&decoded.text, &decoded.format).unwrap(),
            bytes,
            "{}",
            encoding.name()
        );
        count += 1;
    }
    println!(
        "{count} encodings available; unavailable: {}",
        unavailable.join(", ")
    );
    assert!(count > 20);
}

#[test]
fn bom_and_malformed_unicode() {
    for (bytes, encoding, endian, text, lossy) in [
        (
            vec![0xff, 0xfe, 0x61, 0],
            TextEncoding::Utf16,
            Endian::Little,
            "a",
            false,
        ),
        (
            vec![0xfe, 0xff, 0, 0x61],
            TextEncoding::Utf16Be,
            Endian::Big,
            "a",
            false,
        ),
        (
            vec![0xff, 0xfe, 0, 0, 0x61, 0, 0, 0],
            TextEncoding::Utf32,
            Endian::Little,
            "a",
            false,
        ),
        (
            vec![0, 0, 0xfe, 0xff, 0, 0, 0, 0x61],
            TextEncoding::Utf32Be,
            Endian::Big,
            "a",
            false,
        ),
        (
            vec![0xff, 0xfe, 0x61, 0, 0x62],
            TextEncoding::Utf16,
            Endian::Little,
            "a",
            true,
        ),
        (
            vec![0xff, 0xfe, 0x3d, 0xd8, 0x61, 0],
            TextEncoding::Utf16,
            Endian::Little,
            "\u{fffd}a",
            true,
        ),
    ] {
        let d = decode(bytes.clone(), EncodingChoice::Automatic).unwrap();
        assert_eq!(d.text, text);
        assert_eq!(d.format.encoding, encoding);
        assert_eq!(d.format.endian, Some(endian));
        assert_eq!(d.format.lossy, lossy);
        if !lossy {
            assert_eq!(encode(&d.text, &d.format).unwrap(), bytes);
        }
    }
    assert!(matches!(
        decode(
            vec![0xff, 0xfe, 0, 0, 0, 0, 0x11, 0],
            EncodingChoice::Automatic
        ),
        Err(CodecError::InapplicableEncoding(TextEncoding::Utf32))
    ));
    for bytes in [
        vec![0xfe, 0xff, 0, 0x61, 0],
        vec![0xff, 0xfe, 0, 0xdc, 0x61, 0],
        vec![0xfe, 0xff, 0xd8, 0x3d, 0, 0x61],
        vec![0xff, 0xfe, 0, 0, 0x61, 0],
        vec![0xff, 0xfe, 0, 0, 0x61, 0, 0, 0, 0x62, 0],
        vec![0, 0, 0xfe, 0xff, 0, 0, 0, 0x61, 0],
    ] {
        let decoded = decode(bytes.clone(), EncodingChoice::Automatic).unwrap();
        assert!(decoded.format.lossy, "{bytes:?}");
    }
    for bytes in [
        vec![0xff, 0xfe, 0, 0, 0, 0, 0x11, 0, 0x61, 0, 0, 0],
        vec![0, 0, 0xfe, 0xff, 0, 0x11, 0, 0, 0, 0, 0x61],
        vec![0xff, 0xfe, 0, 0, 0, 0xd8, 0, 0],
    ] {
        let result = decode(bytes.clone(), EncodingChoice::Automatic);
        assert!(
            matches!(
                result,
                Err(CodecError::InapplicableEncoding(TextEncoding::Utf32))
            ),
            "{bytes:?}: {result:?}"
        );
    }
}

#[test]
fn explicit_and_line_endings() {
    let d = decode(b"a\r\nb\nc\rd\n".to_vec(), EncodingChoice::Automatic).unwrap();
    assert_eq!(d.format.main_line_ending, LineEnding::Lf);
    assert_eq!(encode(&d.text, &d.format).unwrap(), b"a\r\nb\nc\rd\n");
    assert!(matches!(
        decode(vec![0xff], EncodingChoice::Specific(TextEncoding::Utf8)),
        Err(CodecError::InapplicableEncoding(TextEncoding::Utf8))
    ));
    let d = decode(vec![0, 0x61], EncodingChoice::Specific(TextEncoding::Utf16)).unwrap();
    assert_eq!(d.format.endian, Some(Endian::Big));
    assert_eq!(encode(&d.text, &d.format).unwrap(), vec![0, 0x61]);
    let d = decode(
        vec![0, 0, 0, 0x61],
        EncodingChoice::Specific(TextEncoding::Utf32),
    )
    .unwrap();
    assert_eq!(d.format.endian, Some(Endian::Big));
    assert_eq!(encode(&d.text, &d.format).unwrap(), vec![0, 0, 0, 0x61]);
    assert_eq!(encode("a", &Format::new_document()).unwrap(), b"a");
    let bom = decode(vec![0xef, 0xbb, 0xbf, b'a'], EncodingChoice::Automatic).unwrap();
    assert_eq!(bom.text, "a");
    assert!(bom.format.has_bom);
    assert_eq!(
        encode(&bom.text, &bom.format).unwrap(),
        vec![0xef, 0xbb, 0xbf, b'a']
    );
    assert!(!can_encode("☃", TextEncoding::Windows1252).unwrap());
}

#[test]
fn atomic_failure_keeps_original() {
    let dir = std::env::temp_dir().join(format!("qe-codec-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("original.txt");
    std::fs::write(&path, b"before").unwrap();
    let mut permissions = std::fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&path, permissions.clone()).unwrap();
    assert!(matches!(
        atomic_write(&path, b"after"),
        Err(CodecError::ReadOnly(_))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), b"before");
    permissions.set_readonly(false);
    std::fs::set_permissions(&path, permissions).unwrap();
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

#[test]
fn replace_failure_keeps_original_and_names_fallback_limit() {
    let dir = std::env::temp_dir().join(format!("qe-codec-replace-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("target");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("keep.txt"), b"original").unwrap();
    let result = atomic_write(&target, b"new bytes").unwrap();
    let AtomicWriteResult::ReplacementUnavailable { temp_path, limit } = result else {
        panic!("expected replacement failure")
    };
    assert!(limit.contains("not be atomic"));
    assert_eq!(std::fs::read(target.join("keep.txt")).unwrap(), b"original");
    assert_eq!(std::fs::read(&temp_path).unwrap(), b"new bytes");
    std::fs::remove_file(temp_path).unwrap();
    std::fs::remove_file(target.join("keep.txt")).unwrap();
    std::fs::remove_dir(target).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

#[test]
fn atomic_write_creates_new_file() {
    let dir = std::env::temp_dir().join(format!("qe-codec-new-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("new.txt");
    assert!(matches!(
        atomic_write(&target, b"created"),
        Ok(AtomicWriteResult::Replaced)
    ));
    assert_eq!(std::fs::read(&target).unwrap(), b"created");
    std::fs::remove_file(target).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

#[test]
fn atomic_write_replaces_existing_file() {
    let dir = std::env::temp_dir().join(format!("qe-codec-existing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("existing.txt");
    std::fs::write(&target, b"old").unwrap();
    let created = std::fs::metadata(&target).unwrap().created().unwrap();
    assert!(matches!(
        atomic_write(&target, b"new"),
        Ok(AtomicWriteResult::Replaced)
    ));
    assert_eq!(std::fs::read(&target).unwrap(), b"new");
    assert_eq!(
        std::fs::metadata(&target).unwrap().created().unwrap(),
        created
    );
    std::fs::remove_file(target).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

#[test]
fn streaming_and_cancel() {
    let dir = std::env::temp_dir().join(format!("qe-codec-stream-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.txt");
    let output = dir.join("output.txt");
    let text = "a\r\nβ\r\n".repeat(300_000);
    let bytes = encode(
        &text,
        &Format::new_document().changed_to(TextEncoding::Utf16),
    )
    .unwrap();
    std::fs::write(&input, bytes).unwrap();
    let mut updates = 0;
    let format = qe_codec::transcode_to_utf8_file(
        &input,
        &output,
        EncodingChoice::Automatic,
        |_, _| updates += 1,
        || false,
    )
    .unwrap();
    assert!(updates > 1);
    assert_eq!(std::fs::read_to_string(&output).unwrap(), text);
    assert_eq!(format.main_line_ending, LineEnding::CrLf);
    std::fs::remove_file(&output).unwrap();
    let mut calls = 0;
    assert!(matches!(
        qe_codec::transcode_to_utf8_file(
            &input,
            &output,
            EncodingChoice::Automatic,
            |_, _| {},
            || {
                calls += 1;
                calls > 1
            }
        ),
        Err(CodecError::Cancelled)
    ));
    assert!(!output.exists());
    std::fs::remove_file(input).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

#[test]
fn streaming_shift_jis_crosses_chunk_boundary() {
    if !qe_codec::is_available(TextEncoding::ShiftJis) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("qe-codec-sjis-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input");
    let output = dir.join("output");
    let text = "\u{6f22}".repeat(600_000);
    let bytes = encode(
        &text,
        &Format::new_document().changed_to(TextEncoding::ShiftJis),
    )
    .unwrap();
    std::fs::write(&input, bytes).unwrap();
    let format = qe_codec::transcode_to_utf8_file(
        &input,
        &output,
        EncodingChoice::Specific(TextEncoding::ShiftJis),
        |_, _| {},
        || false,
    )
    .unwrap();
    assert!(!format.lossy);
    assert_eq!(std::fs::read_to_string(&output).unwrap(), text);
    std::fs::remove_file(input).unwrap();
    std::fs::remove_file(output).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

#[test]
fn copy_large_original_preserves_bytes_and_cancels() {
    let dir = std::env::temp_dir().join(format!("qe-codec-copy-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input");
    let output = dir.join("output");
    let bytes = vec![0xa5; 2_200_000];
    std::fs::write(&input, &bytes).unwrap();
    let mut updates = 0;
    qe_codec::copy_large_original(&input, &output, |_, _| updates += 1, || false).unwrap();
    assert!(updates >= 3);
    assert_eq!(std::fs::read(&output).unwrap(), bytes);
    std::fs::remove_file(&output).unwrap();
    let mut calls = 0;
    assert!(matches!(
        qe_codec::copy_large_original(
            &input,
            &output,
            |_, _| {},
            || {
                calls += 1;
                calls > 1
            }
        ),
        Err(CodecError::Cancelled)
    ));
    assert!(!output.exists());
    std::fs::remove_file(input).unwrap();
    std::fs::remove_dir(dir).unwrap();
}
