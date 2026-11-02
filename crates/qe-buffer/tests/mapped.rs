#![cfg(windows)]
use qe_api::{ByteOffset, LineIndex, TextSnapshot, Utf16Offset, testing::StringStore};
use qe_buffer::MappedStore;
use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::time::{Duration, Instant};

fn temp(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("qe-buffer-{}-{name}", std::process::id()))
}

#[test]
fn gib_open_first_screen_and_index() {
    let path = temp("gib.txt");
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    file.set_len(1024 * 1024 * 1024).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(b"first screen\n").unwrap();
    drop(file);
    let start = Instant::now();
    let store = MappedStore::open(&path).unwrap();
    let open_time = start.elapsed();
    let mut first = String::new();
    assert!(store.visit_chunks(ByteOffset(0)..ByteOffset(13), &mut |s| first.push_str(s)));
    assert_eq!(store.line_range(LineIndex(0)).unwrap().end, ByteOffset(13));
    let first_time = start.elapsed();
    assert_eq!(first, "first screen\n");
    while !store.is_index_complete()
        && store.error_state().is_none()
        && start.elapsed() < Duration::from_secs(60)
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    let done = store.is_index_complete();
    let error = store.error_state();
    drop(store);
    fs::remove_file(&path).unwrap();
    eprintln!(
        "1 GiB open {:?}, first screen {:?}, index {:?}",
        open_time,
        first_time,
        start.elapsed()
    );
    assert!(done, "index did not finish within 60 s: {error:?}");
}

#[test]
fn external_truncate_replace_and_save_over() {
    for action in ["truncate", "replace", "save-over"] {
        let path = temp(&format!("{action}.txt"));
        fs::write(&path, "hello\nworld").unwrap();
        let store = MappedStore::open(&path).unwrap();
        match action {
            "truncate" => {
                OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(2)
                    .unwrap();
            }
            "replace" => {
                fs::remove_file(&path).unwrap();
                fs::write(&path, "new file\n!!").unwrap();
            }
            _ => {
                fs::write(&path, "saved over!").unwrap();
            }
        }
        let mut text = String::new();
        assert!(
            !store.visit_chunks(ByteOffset(0)..ByteOffset(5), &mut |s| text.push_str(s)),
            "{action}"
        );
        assert!(store.error_state().is_some(), "{action}");
        drop(store);
        fs::remove_file(path).unwrap();
    }
}

#[test]
fn mapped_positions_match_reference() {
    let path = temp("positions.txt");
    let text = format!("{}\r\n💡\u{2028}last", "x".repeat(65_535));
    fs::write(&path, &text).unwrap();
    let store = MappedStore::open(&path).unwrap();
    let reference = StringStore::new(&text);
    for at in [0, 65_535, 65_536, 65_537, 65_541, text.len()] {
        assert_eq!(
            store.byte_to_utf16(ByteOffset(at)),
            reference.byte_to_utf16(ByteOffset(at))
        );
        assert_eq!(
            store.byte_to_position(ByteOffset(at)),
            reference.byte_to_position(ByteOffset(at)),
            "byte {at}"
        );
    }
    for at in [0, 65_535, 65_536, 65_538, reference.len_utf16().0] {
        assert_eq!(
            store.utf16_to_byte(Utf16Offset(at)),
            reference.utf16_to_byte(Utf16Offset(at))
        );
    }
    for line in 0..reference.line_count() {
        assert_eq!(
            store.line_range(LineIndex(line)),
            reference.line_range(LineIndex(line))
        );
    }
    drop(store);
    fs::remove_file(path).unwrap();
}
