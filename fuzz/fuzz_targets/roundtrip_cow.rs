#![no_main]

use libfuzzer_sys::fuzz_target;
use tristr::TriStr;

/// Core invariants across the inline/heap boundary: construction round-trip,
/// clone sharing, COW isolation, in-place mutation, std conversions.
fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    let v = TriStr::from(s);
    assert_eq!(v.as_str(), s);
    assert_eq!(v.len(), s.len());
    assert_eq!(v.is_inline(), s.len() <= 23);

    // clone shares; originals never observe mutations of a clone
    let c1 = v.clone();
    let c2 = v.clone();
    if s.len() > 23 {
        assert_eq!(v.shared_count(), Some(3));
    } else {
        assert_eq!(v.shared_count(), None);
    }
    assert_eq!(c1.as_str(), s);
    assert_eq!(c2.as_str(), s);

    let mut m = v.clone();
    m.make_ascii_uppercase();
    assert_eq!(v.as_str(), s, "COW: original must be untouched");
    assert_eq!(c2.as_str(), s);

    // unique-owner in-place mutation
    let mut own = TriStr::from(s);
    own.make_ascii_lowercase();
    assert_eq!(own.as_str(), s.to_ascii_lowercase());

    // drop while shared, then verify survivor
    drop(c1);
    drop(m);
    assert_eq!(v.as_str(), s);

    assert_eq!(String::from(v.clone()), s);
    assert_eq!(TriStr::from(s.to_string()).as_str(), s);
});
