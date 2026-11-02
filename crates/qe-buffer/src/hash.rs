//! A small, fast, non-cryptographic hash for catching outside changes to a page.
//! Each 8-byte word goes through a step that cannot cancel on its own, so a change
//! that stays in one 32-byte group always changes that group's lanes.

const P1: u64 = 0x9E37_79B1_85EB_CA87;
const P2: u64 = 0xC2B2_AE3D_27D4_EB4F;

#[inline(always)]
fn step(lane: u64, word: u64) -> u64 {
    (lane ^ word)
        .wrapping_mul(P1)
        .rotate_left(31)
        .wrapping_mul(P2)
}

/// Never returns 0, so 0 can mean "no hash yet".
pub(crate) fn page_hash(bytes: &[u8]) -> u64 {
    let mut lanes = [P1, P2, !P1, !P2];
    let (groups, tail) = bytes.as_chunks::<32>();
    for group in groups {
        let (words, _) = group.as_chunks::<8>();
        for (lane, word) in lanes.iter_mut().zip(words) {
            *lane = step(*lane, u64::from_le_bytes(*word));
        }
    }
    let mut last = [0; 32];
    last[..tail.len()].copy_from_slice(tail);
    let (words, _) = last.as_chunks::<8>();
    for (lane, word) in lanes.iter_mut().zip(words) {
        *lane = step(*lane, u64::from_le_bytes(*word));
    }
    let mut hash = bytes.len() as u64;
    for lane in lanes {
        hash = step(hash, lane);
    }
    (hash ^ (hash >> 29)) | 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_one_bit_flip_changes_the_hash() {
        let mut bytes: Vec<u8> = (0..300u32).map(|i| (i * 7) as u8).collect();
        let base = page_hash(&bytes);
        for at in 0..bytes.len() {
            for bit in 0..8 {
                bytes[at] ^= 1 << bit;
                assert_ne!(page_hash(&bytes), base, "byte {at}, bit {bit}");
                bytes[at] ^= 1 << bit;
            }
        }
    }

    #[test]
    fn length_and_trailing_zeros_change_the_hash() {
        assert_ne!(page_hash(&[]), page_hash(&[0]));
        assert_ne!(page_hash(&[0; 32]), page_hash(&[0; 33]));
        assert_ne!(page_hash(b"abc"), 0);
    }

    #[test]
    fn swapped_words_change_the_hash() {
        let a = b"11111111222222223333333344444444";
        let b = b"22222222111111113333333344444444";
        assert_ne!(page_hash(a), page_hash(b));
    }

    #[test]
    fn the_review_case_of_two_bytes_becoming_one_character_changes_the_hash() {
        let mut page = vec![b'a'; 64 * 1024];
        let base = page_hash(&page);
        page[1000..1002].copy_from_slice("é".as_bytes());
        assert_ne!(page_hash(&page), base);
    }
}
