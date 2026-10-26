#![no_main]

use libfuzzer_sys::fuzz_target;
use std::collections::HashMap;
use tristr::TriStr;

/// HashMap/Borrow/Hash/Ord semantics must match std String exactly.
fuzz_target!(|data: &[u8]| {
    let mut strs: Vec<&str> = vec![];
    for chunk in data.chunks(7) {
        if let Ok(s) = std::str::from_utf8(chunk) {
            strs.push(s);
        }
    }

    let mut map: HashMap<TriStr, u32> = HashMap::new();
    let mut reference: HashMap<String, u32> = HashMap::new();
    for (i, s) in strs.iter().enumerate() {
        map.insert(TriStr::from(*s), i as u32);
        reference.insert(s.to_string(), i as u32);
    }

    for s in &strs {
        assert_eq!(map.get(*s), reference.get(s.to_string().as_str()));
    }

    // total order parity
    let mut a: Vec<TriStr> = strs.iter().map(|s| TriStr::from(*s)).collect();
    a.sort();
    let mut b: Vec<String> = strs.iter().map(|s| s.to_string()).collect();
    b.sort();
    for (x, y) in a.iter().zip(&b) {
        assert_eq!(x.as_str(), y.as_str());
        assert_eq!(x, y);
    }

    // removal + survivor integrity
    if let Some(first) = strs.first() {
        map.remove(*first);
        assert!(map.get(*first).is_none());
    }
    for (k, v) in &map {
        assert_eq!(reference.get(k.as_str()), Some(v));
    }
});
