//! Deterministic deep-path exercise: a small PRNG drives construction, clone
//! sharing, COW mutation, conversion and map operations across the inline/heap
//! boundary. Runs under cargo test and Miri (no libfuzzer, no wall-clock).

use std::collections::HashMap;
use std::hint::black_box;
use tristr::TriStr;

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }
    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn ascii_string(&mut self, len: usize) -> String {
        (0..len)
            .map(|_| (b'a' + (self.below(26) as u8)) as char)
            .collect()
    }
}

#[test]
fn deep_paths_roundtrip_cow_and_maps() {
    let mut rng = Rng::new(0xC0FFEE);

    for _ in 0..1500u32 {
        // Lengths 0..63 deliberately cross the 23-byte inline boundary.
        let len = rng.below(64) as usize;
        let s = rng.ascii_string(len);
        let expected = s.clone();

        let v = TriStr::from(s.as_str());
        assert_eq!(v.as_str(), expected);
        assert_eq!(v.is_inline(), len <= 23);

        // Random fan-out of clones; sharing state must be exact.
        let fan = rng.below(4);
        let mut clones = Vec::new();
        for _ in 0..fan {
            clones.push(v.clone());
        }
        if len > 23 {
            assert_eq!(v.shared_count(), Some(1 + fan as usize));
        }

        // Mutate one clone; every other handle must observe the original.
        if let Some(first) = clones.first_mut() {
            first.make_ascii_uppercase();
            assert_eq!(first.as_str(), expected.to_ascii_uppercase());
        }
        assert_eq!(v.as_str(), expected);
        for c in clones.iter().skip(1) {
            assert_eq!(c.as_str(), expected);
        }
        drop(clones);

        // Unique owner mutates in place.
        let mut own = TriStr::from(s.as_str());
        own.make_ascii_lowercase();
        assert_eq!(own.as_str(), expected.to_ascii_lowercase());

        // Conversions both ways.
        assert_eq!(String::from(TriStr::from(s.as_str())), expected);
        assert_eq!(TriStr::from(expected.clone()).as_str(), expected);
    }
}

#[test]
fn deep_paths_map_agreement() {
    let mut rng = Rng::new(0xBADF00D);
    let mut map: HashMap<TriStr, u32> = HashMap::new();
    let mut reference: HashMap<String, u32> = HashMap::new();

    for i in 0..600u32 {
        let len = rng.below(40) as usize;
        let s = rng.ascii_string(len);
        let op = rng.below(3);
        match op {
            0 | 1 => {
                map.insert(TriStr::from(s.as_str()), i);
                reference.insert(s, i);
            }
            _ => {
                map.remove(s.as_str());
                reference.remove(s.as_str());
            }
        }
    }

    assert_eq!(map.len(), reference.len());
    for (k, v) in &map {
        assert_eq!(reference.get(k.as_str()), Some(v));
    }
    black_box(&map);
}

#[test]
fn deep_paths_boundary_lengths() {
    // Every length around the inline/heap seam, with multibyte content.
    for len in 0..=48usize {
        let s: String = "é".repeat(len); // 2 bytes per char
        let bytes = s.len();
        let v = TriStr::from(s.as_str());
        assert_eq!(v.as_str(), s);
        assert_eq!(v.is_inline(), bytes <= 23);
        let c = v.clone();
        assert_eq!(c.as_str(), s);
        drop(v);
        assert_eq!(c.as_str(), s);
    }
}

#[test]
fn formatting_surface() {
    use std::fmt::Write as _;

    // write! on a fresh value, short (inline) path
    let mut v = TriStr::new();
    write!(v, "{}-{}", "key", 12345).unwrap();
    assert_eq!(v.as_str(), "key-12345");
    assert!(v.is_inline());

    // push_str crossing the 23B seam: content preserved, becomes heap
    let mut long = TriStr::new();
    long.push_str("0123456789");
    long.push_str("0123456789");
    assert!(long.is_inline());
    assert_eq!(long.as_str().len(), 20);
    long.push_str("abc"); // 23: still inline
    assert!(long.is_inline());
    long.push_str("d"); // 24: materializes heap
    assert_eq!(long.as_str(), "01234567890123456789abcd");
    assert!(!long.is_inline());

    // push(char) incl. multibyte
    let mut mb = TriStr::new();
    mb.push('é');
    mb.push('🦀');
    assert_eq!(mb.as_str(), "é🦀");

    // Add / AddAssign
    let joined = TriStr::from("ab") + "cd";
    assert_eq!(joined.as_str(), "abcd");
    let mut aug = TriStr::from("ab");
    aug += "cd";
    assert_eq!(aug.as_str(), "abcd");

    // FromIterator<&str> and <char>
    let it: TriStr = ["a", "b", "c"].into_iter().collect();
    assert_eq!(it.as_str(), "abc");
    let ch: TriStr = "xyz".chars().collect();
    assert_eq!(ch.as_str(), "xyz");

    // COW isolation: pushing to a shared heap value must materialize a
    // private copy and never write into the shared allocation.
    let shared = TriStr::from("x".repeat(64).as_str());
    let snapshot = shared.clone();
    let mut mutated = shared.clone();
    mutated.push_str("+tail");
    assert_eq!(shared.as_str(), "x".repeat(64));
    assert_eq!(snapshot.as_str(), "x".repeat(64));
    assert_eq!(mutated.as_str(), format!("{}+tail", "x".repeat(64)));
}

#[test]
fn eq_fast_path_semantics() {
    // equal content, both inline
    assert!(TriStr::from("mixed-port") == TriStr::from("mixed-port"));
    // same length, different last byte (fast-path memcmp territory)
    assert!(TriStr::from("mixed-porz") != TriStr::from("mixed-port"));
    // different lengths, same prefix (tag inequality shortcut)
    assert!(TriStr::from("mixed") != TriStr::from("mixed-port"));
    // both heap
    let long_a = "x".repeat(64);
    let long_b = "x".repeat(64);
    let long_c = "y".to_owned() + &"x".repeat(63);
    assert!(TriStr::from(long_a.as_str()) == TriStr::from(long_b.as_str()));
    assert!(TriStr::from(long_a.as_str()) != TriStr::from(long_c.as_str()));
    // inline vs heap with same content-length mismatch and equality
    let h = TriStr::from(long_a.as_str());
    let i = TriStr::from("short");
    assert!(h != i);
    // hash consistency with eq (Borrow contract)
    use std::hash::{Hash, Hasher};
    let hash_of = |v: &TriStr| {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        v.hash(&mut h);
        h.finish()
    };
    assert_eq!(hash_of(&h), {
        let mut h2 = std::collections::hash_map::DefaultHasher::new();
        long_a.as_str().hash(&mut h2);
        h2.finish()
    });
}

#[test]
fn ord_fast_path_matches_str_semantics() {
    let cases = [
        ("ab", "b"),                  // prefix byte decides ("a" < "b")
        ("b", "ab"),                  // mirrored
        ("ab", "abc"),                // prefix equal, shorter is less
        ("abc", "ab"),                // mirrored
        ("abc", "abc"),               // equal
        ("mixed-porz", "mixed-port"), // last byte decides
        ("", "a"),                    // empty vs one
        ("a", ""),                    // mirrored
        ("é", "z"),                   // multibyte byte-order (0xC3 > 'z')
    ];
    for (a, b) in cases {
        let (va, vb) = (TriStr::from(a), TriStr::from(b));
        assert_eq!(
            va.cmp(&vb),
            a.cmp(b),
            "Ord fast path diverged from str for {a:?} vs {b:?}"
        );
        assert_eq!(va.partial_cmp(&vb), a.partial_cmp(b));
        // sort stability agreement with std on a shuffled multiset
    }
    let mut v: Vec<TriStr> = ["b", "abc", "", "ab", "a", "b", "abc"]
        .into_iter()
        .map(TriStr::from)
        .collect();
    v.sort();
    let expected = ["", "a", "ab", "abc", "abc", "b", "b"];
    for (x, y) in v.iter().zip(expected) {
        assert_eq!(x.as_str(), y);
    }
    // heap-vs-inline and heap-vs-heap fall back correctly
    let h1 = TriStr::from("x".repeat(64).as_str());
    let h2 = TriStr::from("y".repeat(63).as_str());
    let i1 = TriStr::from("short");
    assert_eq!(
        h1.cmp(&h2),
        "x".repeat(64).as_str().cmp("y".repeat(63).as_str())
    );
    assert_eq!(h1.cmp(&i1), core::cmp::Ordering::Greater);
    assert_eq!(i1.cmp(&h1), core::cmp::Ordering::Less);
}

#[test]
fn capacity_mirrors_smartstring_semantics() {
    let i = TriStr::from("short");
    assert_eq!(i.capacity(), 23);
    assert!(i.is_inline());
    let mut i2 = i.clone();
    i2.shrink_to_fit(); // no-op, must not change anything
    assert_eq!(i2.as_str(), "short");

    let h = TriStr::from("x".repeat(64).as_str());
    assert_eq!(h.capacity(), 64);
    let mut h2 = h.clone();
    h2.shrink_to_fit();
    assert_eq!(h2.as_str(), "x".repeat(64));
    assert_eq!(h2.shared_count(), Some(2), "shrink_to_fit must not unshare");
}

#[test]
fn partialord_ltgt_matches_str() {
    let cases = [
        ("ab", "b"),
        ("b", "ab"),
        ("ab", "abc"),
        ("abc", "ab"),
        ("same", "same"),
        ("mixed-porz", "mixed-port"),
        ("", "a"),
        ("a", ""),
    ];
    for (a, b) in cases {
        let (va, vb) = (TriStr::from(a), TriStr::from(b));
        assert_eq!(va < vb, a < b, "lt mismatch: {a:?} vs {b:?}");
        assert_eq!(va > vb, a > b, "gt mismatch: {a:?} vs {b:?}");
        assert_eq!(va <= vb, a <= b);
        assert_eq!(va >= vb, a >= b);
    }
    let h1 = TriStr::from("x".repeat(64).as_str());
    let i1 = TriStr::from("short");
    assert!(i1 < h1 && h1 > i1);
}

#[test]
fn boxed_third_state() {
    let s = "x".repeat(64); // exact capacity
    let v = TriStr::from(s.clone());
    assert!(v.is_boxed());
    assert!(!v.is_inline());
    assert_eq!(v.shared_count(), None);
    assert_eq!(v.as_str(), s);
    assert_eq!(v.capacity(), 64);

    // owned mutates in place, stays owned
    let mut m = TriStr::from(s.clone());
    m.make_ascii_uppercase();
    assert_eq!(m.as_str(), "X".repeat(64));
    assert!(m.is_boxed());

    // clone deep-copies without touching self; the copy is shared
    let c = v.clone();
    assert!(!c.is_boxed());
    assert_eq!(c.shared_count(), Some(1));
    let mut m2 = c.clone();
    m2.make_ascii_uppercase();
    assert_eq!(v.as_str(), s, "owned original untouched");

    // zero-copy round trip back to String
    let back: String = TriStr::from(s.clone()).into();
    assert_eq!(back, s);

    // slack-capacity strings take the shared path
    let mut slack = String::with_capacity(128);
    slack.push_str(&"y".repeat(64));
    let sv = TriStr::from(slack);
    assert!(!sv.is_boxed());
    assert_eq!(sv.shared_count(), Some(1));

    // push_str on a clone materializes a private copy; shared original intact
    let shared = TriStr::from("z".repeat(64).as_str());
    let mut p = shared.clone();
    p.push_str("+tail");
    assert_eq!(shared.as_str(), "z".repeat(64));
    assert!(p.as_str().ends_with("+tail"));

    // equality/order across all three reprs is content-based
    let b = TriStr::from(s.clone()); // boxed
    let a = TriStr::from(s.as_str()); // shared
    let i = TriStr::from("short"); // inline
    assert!(b == a && a == b);
    assert!(i != b && b != i);
    assert_eq!(b.cmp(&a), core::cmp::Ordering::Equal);
}

#[test]
fn deser_routes_to_owned() {
    #[derive(serde::Serialize, serde::Deserialize)]
    struct W {
        short: TriStr,
        long: TriStr,
    }
    let w = W {
        short: TriStr::from("key"),
        long: TriStr::from("x".repeat(300).as_str()),
    };
    let j = serde_json::to_string(&w).unwrap();
    let back: W = serde_json::from_str(&j).unwrap();
    assert!(back.short.is_inline());
    assert!(back.long.is_boxed(), "long deserialized values are owned");
    assert_eq!(back.short.as_str(), "key");
    assert_eq!(back.long.as_str(), "x".repeat(300));

    // owned clone deep-copies; shared original intact
    let c = back.long.clone();
    assert!(!c.is_boxed());
    assert_eq!(back.long.as_str(), "x".repeat(300));

    // yaml path too
    let y = serde_yaml_ng::to_string(&w).unwrap();
    let back2: W = serde_yaml_ng::from_str(&y).unwrap();
    assert!(back2.long.is_boxed());
    assert_eq!(back2.long.as_str(), "x".repeat(300));
}
