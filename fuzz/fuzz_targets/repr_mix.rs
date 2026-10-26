#![no_main]

use libfuzzer_sys::fuzz_target;
use std::collections::{BTreeMap, HashMap};
use std::hint::black_box;
use tristr::TriStr;

// HashMap/BTreeMap with deliberately mixed-representation keys: the same
// logical string enters as inline (short &str), owned (exact-capacity
// String) and shared (slack-capacity String / cloned Arc) keys. Cross-repr
// lookup, hit/miss, removal, iteration order and equality are checked
// against std `String` reference maps after every operation — the
// `Borrow<str>`/`Hash`/`Eq`/`Ord` contract across all three reprs.
fuzz_target!(|data: &[u8]| {
    // Deterministic key strings, 0..48 bytes (crossing the 23B seam).
    let mut keys: Vec<String> = vec![];
    for chunk in data.chunks(6) {
        let Ok(s) = std::str::from_utf8(chunk) else {
            continue;
        };
        keys.push(s.to_string());
    }
    if keys.is_empty() {
        return;
    }

    let mut map: HashMap<TriStr, u32> = HashMap::new();
    let mut bmap: BTreeMap<TriStr, u32> = BTreeMap::new();
    let mut ref_h: HashMap<String, u32> = HashMap::new();
    let mut ref_b: BTreeMap<String, u32> = BTreeMap::new();

    // Insert each logical key through a rotating repr route, so equal
    // content frequently arrives in different reprs (including duplicate
    // content overwriting via another repr).
    let route_for = |i: usize, s: &str| -> TriStr {
        match (i + s.len()) % 3 {
            0 => TriStr::from(s),                   // inline or shared
            1 => TriStr::from(s.to_owned()),        // inline or owned (exact-cap)
            _ => {
                let mut slack = String::with_capacity(s.len() + 32);
                slack.push_str(s);
                TriStr::from(slack) // inline or shared
            }
        }
    };

    for i in 0..keys.len() {
        let k = route_for(i, &keys[i % keys.len()]);
        let rk = keys[i % keys.len()].clone();
        let val = (data[i % data.len()] as u32) << 8 | i as u32;
        assert_eq!(map.insert(k.clone(), val), ref_h.insert(rk.clone(), val), "hash insert ret ({i})");
        assert_eq!(bmap.insert(k, val), ref_b.insert(rk, val), "btree insert ret ({i})");
    }

    // Cross-repr lookups: every key, through every construction route,
    // resolves identically in both maps (Borrow<str> + Hash + Eq + Ord).
    for i in 0..keys.len() {
        let s = &keys[i % keys.len()];
        for probe in [
            TriStr::from(s.as_str()),
            TriStr::from(s.clone()),
            TriStr::from({
                let mut slack = String::with_capacity(s.len() + 32);
                slack.push_str(s);
                slack
            }),
            route_for(i + 1, s),
        ] {
            assert_eq!(map.get(&probe), ref_h.get(s), "hash get repr-mix ({i})");
            assert_eq!(bmap.get(&probe), ref_b.get(s), "btree get repr-mix ({i})");
            // &str-keyed lookup too (Borrow dynamic dispatch path)
            assert_eq!(map.get(s.as_str()), ref_h.get(s), "hash get &str ({i})");
        }
    }

    // Misses agree.
    let miss = format!("{}-absent", data.len());
    assert_eq!(map.contains_key(miss.as_str()), ref_h.contains_key(miss.as_str()));
    assert_eq!(bmap.contains_key(miss.as_str()), ref_b.contains_key(miss.as_str()));

    // Length agreement, then removals in data-driven order through a
    // different repr than insertion used.
    assert_eq!(map.len(), ref_h.len());
    assert_eq!(bmap.len(), ref_b.len());
    let mut order: Vec<usize> = (0..keys.len()).collect();
    let mut prng = data.iter().fold(0x9E3779B97F4A7C15u64, |a, &b| {
        a ^ ((b as u64).wrapping_mul(0x100000001B3))
    });
    for i in (1..order.len()).rev() {
        prng ^= prng << 13;
        prng ^= prng >> 7;
        prng ^= prng << 17;
        order.swap(i, (prng % (i as u64 + 1)) as usize);
    }
    for &i in &order {
        let s = &keys[i % keys.len()];
        let probe = route_for(i + 1, s); // same content, different repr route
        if s.is_empty() {
            continue; // empty-string keys: all routes identical by design
        }
        assert_eq!(map.remove(&probe), ref_h.remove(s), "hash remove ({i})");
        assert_eq!(bmap.remove(&probe), ref_b.remove(s), "btree remove ({i})");
    }
    assert_eq!(map.len(), ref_h.len());
    assert_eq!(bmap.len(), ref_b.len());

    // Full-iteration parity: every surviving key/value pair matches the
    // reference exactly (BTreeMap already checks order via Ord; the sorted
    // sequences must be identical too).
    for (k, v) in &map {
        assert_eq!(ref_h.get(k.as_str()), Some(v), "hash iter parity");
    }
    let b_keys: Vec<&str> = bmap.keys().map(|k| k.as_str()).collect();
    let rb_keys: Vec<&str> = ref_b.keys().map(|k| k.as_str()).collect();
    assert_eq!(b_keys, rb_keys, "btree iteration order parity");
    black_box((&map, &bmap));
});
