#![no_main]

use libfuzzer_sys::fuzz_target;
use std::borrow::Cow;
use std::str::FromStr;
use tristr::TriStr;

/// Every std-mirroring conversion surface (the ones the clash-verge-rev
/// migration depends on) across the inline/heap boundary.
fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    // FromIterator<String>
    let parts: Vec<String> = s.split('\x1f').map(|p| p.to_string()).collect();
    let joined: TriStr = parts.clone().into_iter().collect();
    assert_eq!(joined.as_str(), parts.join(""));

    // From<Cow<str>> both directions
    assert_eq!(TriStr::from(Cow::Borrowed(s)).as_str(), s);
    assert_eq!(TriStr::from(Cow::<str>::Owned(s.to_string())).as_str(), s);

    // From<&std String>
    let std_s = s.to_string();
    assert_eq!(TriStr::from(&std_s).as_str(), s);

    // PartialEq<String> both directions
    assert!(TriStr::from(s) == std_s);
    assert!(std_s == TriStr::from(s));

    // AsRef<[u8]>
    assert_eq!(<TriStr as AsRef<[u8]>>::as_ref(&TriStr::from(s)), s.as_bytes());

    // FromStr
    assert_eq!(s.parse::<TriStr>().unwrap().as_str(), s);

    // new / Default / empty
    assert_eq!(TriStr::new().as_str(), "");
    assert_eq!(TriStr::default(), TriStr::new());

    // Display / Debug formatting parity
    assert_eq!(format!("{}", TriStr::from(s)), s);
    assert_eq!(format!("{:?}", TriStr::from(s)), format!("{:?}", s));

    // hash parity with &str
    use std::hash::{Hash, Hasher};
    let h1 = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        TriStr::from(s).hash(&mut h);
        h.finish()
    };
    let h2 = {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut h);
        h.finish()
    };
    assert_eq!(h1, h2);
});
