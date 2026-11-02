use qe_api::testing::StringStore;
use qe_api::{ByteOffset, LineIndex, TextSnapshot, TextStore, Utf16Offset};
use qe_buffer::{PieceStore, UndoHistory};

#[test]
fn typing_groups_and_memory() {
    let mut store = PieceStore::new("");
    let mut history = UndoHistory::new();
    for i in 0..10_000 {
        history
            .replace(&mut store, ByteOffset(i)..ByteOffset(i), "x", true)
            .unwrap();
    }
    assert_eq!(history.undo_groups(), 1);
    assert!(history.memory_bytes() < 1_000_000);
    history.save_point();
    history
        .replace(
            &mut store,
            ByteOffset(10_000)..ByteOffset(10_000),
            "y",
            true,
        )
        .unwrap();
    assert_eq!(history.undo_groups(), 2);
    history.undo(&mut store).unwrap();
    assert_eq!(store.len_bytes(), ByteOffset(10_000));
    history.undo(&mut store).unwrap();
    assert_eq!(store.len_bytes(), ByteOffset(0));
    history.redo(&mut store).unwrap();
    assert_eq!(store.len_bytes(), ByteOffset(10_000));
}

#[test]
fn random_edits_match_reference() {
    let seed = 0x91a4_723c_589e_1107_u64;
    let mut rng = seed;
    let mut next = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let mut store = PieceStore::new("");
    let mut reference = StringStore::new("");
    let choices = ["x", "💡", "é", "\r", "\n", "\r\n", "\u{2028}", "\u{85}"];
    for step in 0..200_000 {
        let chars: Vec<usize> = reference
            .as_str()
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(reference.len_bytes().0))
            .collect();
        let a = (next() as usize) % chars.len();
        let b = if (chars.len() > 800 && next() % 3 == 0) || next() % 4 == 0 {
            (a + 1).min(chars.len() - 1)
        } else {
            a
        };
        let (start, end) = (chars[a.min(b)], chars[a.max(b)]);
        let insert = if chars.len() > 800 {
            ""
        } else {
            choices[next() as usize % choices.len()]
        };
        let range = ByteOffset(start)..ByteOffset(end);
        let left = store.replace(range.clone(), insert).unwrap();
        let right = reference.replace(range, insert).unwrap();
        assert_eq!(
            left.line_count_delta, right.line_count_delta,
            "seed {seed:x}, step {step}"
        );
        if step % 200 == 0 {
            assert_eq!(
                store.len_bytes(),
                reference.len_bytes(),
                "seed {seed:x}, step {step}"
            );
            assert_eq!(
                store.len_utf16(),
                reference.len_utf16(),
                "seed {seed:x}, step {step}"
            );
            assert_eq!(
                store.line_count(),
                reference.line_count(),
                "seed {seed:x}, step {step}"
            );
            for at in 0..=reference.len_bytes().0 {
                assert_eq!(
                    store.byte_to_utf16(ByteOffset(at)),
                    reference.byte_to_utf16(ByteOffset(at)),
                    "seed {seed:x}, step {step}, byte {at}"
                );
                assert_eq!(
                    store.byte_to_position(ByteOffset(at)),
                    reference.byte_to_position(ByteOffset(at)),
                    "seed {seed:x}, step {step}, byte {at}"
                );
            }
            for at in 0..=reference.len_utf16().0 {
                assert_eq!(
                    store.utf16_to_byte(Utf16Offset(at)),
                    reference.utf16_to_byte(Utf16Offset(at)),
                    "seed {seed:x}, step {step}, unit {at}"
                );
            }
            for at in 0..reference.line_count() {
                assert_eq!(
                    store.line_range(LineIndex(at)),
                    reference.line_range(LineIndex(at)),
                    "seed {seed:x}, step {step}, line {at}"
                );
            }
        }
    }
}

#[test]
fn boundaries_and_snapshot() {
    let mut store = PieceStore::new("a\r\n💡\u{2028}z");
    let old = store.snapshot();
    let reference = StringStore::new("a\r\n💡\u{2028}z");
    for byte in 0..=reference.len_bytes().0 {
        assert_eq!(
            store.byte_to_position(ByteOffset(byte)),
            reference.byte_to_position(ByteOffset(byte))
        );
        assert_eq!(
            store.byte_to_utf16(ByteOffset(byte)),
            reference.byte_to_utf16(ByteOffset(byte))
        );
    }
    for unit in 0..=reference.len_utf16().0 {
        assert_eq!(
            store.utf16_to_byte(Utf16Offset(unit)),
            reference.utf16_to_byte(Utf16Offset(unit))
        );
    }
    for line in 0..reference.line_count() {
        assert_eq!(
            store.line_range(LineIndex(line)),
            reference.line_range(LineIndex(line))
        );
    }
    store.replace(ByteOffset(1)..ByteOffset(1), "x").unwrap();
    assert_eq!(old.len_bytes(), reference.len_bytes());
    std::thread::spawn(move || assert_eq!(old.line_count(), 3))
        .join()
        .unwrap();
}

#[test]
fn crlf_across_original_and_added_piece_boundaries() {
    let text = format!("{}\r\nlast", "x".repeat(4095));
    let mut store = PieceStore::new(&text);
    assert_eq!(store.line_count(), 2);
    assert_eq!(
        store.line_range(LineIndex(1)).unwrap().start,
        ByteOffset(4097)
    );
    store
        .replace(ByteOffset(4096)..ByteOffset(4096), "y")
        .unwrap();
    assert_eq!(store.line_count(), 3);
    store
        .replace(ByteOffset(4096)..ByteOffset(4097), "")
        .unwrap();
    assert_eq!(store.line_count(), 2);
    assert_eq!(
        store.line_range(LineIndex(1)).unwrap().start,
        ByteOffset(4097)
    );
}
