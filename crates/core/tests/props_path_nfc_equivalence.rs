//! Property: `MediaPath::new` collapses canonically-equivalent
//! inputs. Applying NFD decomposition before construction must
//! yield the same `MediaPath`.

use perima_core::MediaPath;
use proptest::prelude::*;
use unicode_normalization::UnicodeNormalization;

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]
    #[test]
    fn media_path_nfc_equivalence(s in "\\PC{0,200}") {
        let nfd: String = s.nfd().collect();
        let from_original = MediaPath::new(&s);
        let from_nfd = MediaPath::new(&nfd);
        prop_assert_eq!(from_original, from_nfd);
    }
}
