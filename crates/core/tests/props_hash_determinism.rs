//! Property: the same byte input always produces the same
//! `BlakeHash`.

use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]
    #[test]
    fn blake_hash_is_deterministic(bytes in proptest::collection::vec(any::<u8>(), 0..2048)) {
        let a = blake3::hash(&bytes);
        let b = blake3::hash(&bytes);
        prop_assert_eq!(a.as_bytes(), b.as_bytes());
    }
}
