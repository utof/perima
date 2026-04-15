//! Property: `MediaPath::new` is idempotent under repeated
//! application — `f(f(s)) == f(s)` for every input.

use perima_core::MediaPath;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]
    #[test]
    fn media_path_is_idempotent(s in "\\PC{0,200}") {
        let once = MediaPath::new(&s);
        let twice = MediaPath::new(once.as_str());
        prop_assert_eq!(once, twice);
    }
}
