#![no_main]

use libfuzzer_sys::fuzz_target;
use std::hash::{Hash, Hasher};
use tristr::TriStr;

/// Three-state repr state machine: inline / owned (Box, adopted from
/// exact-capacity Strings) / shared (Arc, from &str or slack-capacity
/// Strings), plus every transition and cross-repr interaction.
fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    // Construction routes pick reprs by length and capacity.
    let from_ref = TriStr::from(s); // &str: inline or shared
    assert_eq!(from_ref.is_inline(), s.len() <= 23);
    if s.len() > 23 {
        assert_eq!(from_ref.shared_count(), Some(1));
    }

    let exact = s.to_owned(); // capacity == len -> owned
    let owned = TriStr::from(exact.clone());
    if s.len() > 23 {
        assert!(owned.is_boxed());
        assert_eq!(owned.shared_count(), None);
    } else {
        assert!(owned.is_inline());
    }

    let mut slack = String::with_capacity(s.len() + 64);
    slack.push_str(s);
    let shared2 = TriStr::from(slack);
    if s.len() > 23 {
        assert!(!shared2.is_boxed());
        assert_eq!(shared2.shared_count(), Some(1));
    }

    // Content agreement across all three reprs.
    assert!(from_ref == owned && owned == shared2 && from_ref == shared2);
    assert_eq!(from_ref.cmp(&owned), core::cmp::Ordering::Equal);

    // Owned clone deep-copies; the copy is shared; originals untouched.
    let c1 = owned.clone();
    if s.len() > 23 {
        assert!(!c1.is_boxed());
        assert_eq!(c1.shared_count(), Some(1));
    }
    let mut c2 = c1.clone();
    c2.make_ascii_uppercase();
    assert_eq!(owned.as_str(), s);
    assert_eq!(c1.as_str(), s);
    assert_eq!(c2.as_str(), s.to_ascii_uppercase());

    // Owned mutates in place and stays owned.
    let mut m = TriStr::from(exact.clone());
    m.make_ascii_lowercase();
    if s.len() > 23 {
        assert!(m.is_boxed());
    }
    assert_eq!(m.as_str(), s.to_ascii_lowercase());

    // Zero-copy hand-back: String::from(owned) round-trips.
    let back: String = TriStr::from(exact.clone()).into();
    assert_eq!(back, s);

    // Cross-repr mutation isolation: shared clones never observe writes.
    let base = TriStr::from(s);
    let snap = base.clone();
    let mut wr = base.clone();
    wr.push_str("+tail");
    assert_eq!(base.as_str(), s);
    assert_eq!(snap.as_str(), s);
    assert_eq!(wr.as_str(), format!("{s}+tail"));

    // Hash agreement (Borrow<str> contract) across reprs.
    let h = |v: &TriStr| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        v.hash(&mut h);
        h.finish()
    };
    assert_eq!(h(&from_ref), {
        let mut hh = std::collections::hash_map::DefaultHasher::new();
        s.hash(&mut hh);
        hh.finish()
    });
    assert_eq!(h(&from_ref), h(&owned));
    assert_eq!(h(&owned), h(&shared2));

    // Drop chains of every repr (leaks/crashes surface under ASAN/miri).
    drop((from_ref, owned, shared2, c1, c2, m, back));
});
