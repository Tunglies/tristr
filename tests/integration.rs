use std::collections::HashMap;
use std::hint::black_box;
use std::time::Instant;
use tristr::TriStr;

#[test]
fn inline_threshold() {
    assert!(TriStr::from("a".repeat(23).as_str()).is_inline());
    assert!(!TriStr::from("a".repeat(24).as_str()).is_inline());
    assert_eq!(TriStr::from("x").shared_count(), None);
}

#[test]
fn every_length_roundtrips() {
    // The register-built construction places the tail bytes with
    // length-dependent shifts; check every boundary including the
    // shift-overflow guards at len 4/8/16.
    let mut buf = Vec::new();
    for len in 0..=24usize {
        buf.clear();
        buf.extend((0..len).map(|i| b'a' + (i % 26) as u8));
        let s = std::str::from_utf8(&buf).unwrap();
        let v = TriStr::from(s);
        assert_eq!(v.as_str(), s, "len {len}");
        assert_eq!(v.clone().as_str(), s, "clone len {len}");
        assert_eq!(String::from(v), s, "convert len {len}");
    }
}

fn long_string() -> String {
    "ERROR: profile download failed, falling back to cached copy".repeat(4)
}

#[test]
fn unicode_roundtrip() {
    for s in [
        "",
        "mixed-port",
        "你好，世界",
        "🦀🦀🦀",
        &"é".repeat(23),
        &long_string(),
    ] {
        assert_eq!(TriStr::from(s).as_str(), s);
    }
}

#[test]
fn clone_sharing_and_cow() {
    let long = TriStr::from(long_string().as_str());
    assert!(!long.is_inline());

    let mut copy = long.clone();
    assert_eq!(long.shared_count(), Some(2), "clone must share the Arc");

    copy.make_ascii_uppercase();
    assert!(copy
        .as_str()
        .chars()
        .all(|c| c.is_ascii_uppercase() || !c.is_ascii_alphabetic()));
    assert_eq!(long.as_str(), long_string(), "COW: original untouched");
    assert_eq!(
        long.shared_count(),
        Some(1),
        "COW released the shared handle"
    );
}

#[test]
fn cow_promotes_short_shared_to_inline() {
    // Short strings are always inline in this design, so force the heap
    // variant via a long string that shrinks after mutation is not possible
    // (str cannot change length in place) — instead verify a shared short
    // string copied through COW lands inline.
    let shared = TriStr::from("Mixed-Case-Config-Key");
    assert!(shared.is_inline());
    let mut copy = shared.clone();
    copy.make_ascii_lowercase();
    assert_eq!(copy.as_str(), "mixed-case-config-key");
    assert_eq!(shared.as_str(), "Mixed-Case-Config-Key");
}

#[test]
fn inline_in_place_mutation() {
    let mut k = TriStr::from("Mixed-Port");
    k.make_ascii_lowercase();
    assert_eq!(k.as_str(), "mixed-port");
    assert!(k.is_inline(), "short mutation stays inline");

    // multi-byte UTF-8 must pass through the register fold unchanged
    let mut u = TriStr::from("MiXeD-é🦀z");
    u.make_ascii_uppercase();
    assert_eq!(u.as_str(), "MIXED-é🦀Z");
    u.make_ascii_lowercase();
    assert_eq!(u.as_str(), "mixed-é🦀z");
}

#[test]
fn map_key_semantics() {
    let mut m = HashMap::new();
    m.insert(TriStr::from("proxy-groups"), 1u8);
    m.insert(TriStr::from(long_string().as_str()), 2u8);
    assert_eq!(m.get("proxy-groups"), Some(&1));
    assert_eq!(m.get(long_string().as_str()), Some(&2));
}

#[test]
fn ordering_display_debug() {
    let mut v = vec![TriStr::from("b"), TriStr::from("a"), TriStr::from("c")];
    v.sort();
    assert_eq!(
        v.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        ["a", "b", "c"]
    );
    assert_eq!(format!("{}", TriStr::from("hi")), "hi");
    assert_eq!(format!("{:?}", TriStr::from("hi")), "\"hi\"");
}

#[test]
fn from_std_conversions() {
    let s = String::from("format!().into() path");
    let v = TriStr::from(s.clone());
    assert_eq!(v.as_str(), s);
    assert_eq!(String::from(v.clone()), s);
    let long = long_string();
    assert_eq!(TriStr::from(long.clone()).as_str(), long);
    assert_eq!(String::from(TriStr::from(long.clone())), long);
}

#[test]
fn drop_decrements_shares() {
    let long = TriStr::from(long_string().as_str());
    {
        let c1 = long.clone();
        let c2 = long.clone();
        assert_eq!(long.shared_count(), Some(3));
        drop(c1);
        assert_eq!(long.shared_count(), Some(2));
        drop(c2);
    }
    assert_eq!(long.shared_count(), Some(1));
}

#[test]
fn serde_json_parity_with_std_string() {
    #[derive(serde::Serialize, serde::Deserialize)]
    struct A {
        name: TriStr,
        path: TriStr,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    struct B {
        name: String,
        path: String,
    }
    for (name, path) in [
        ("short", "x".repeat(23)),
        ("exact-boundary", "y".repeat(24)),
        ("long", long_string()),
    ] {
        let a = serde_json::to_string(&A {
            name: TriStr::from(name),
            path: TriStr::from(path.as_str()),
        })
        .unwrap();
        let b = serde_json::to_string(&B {
            name: name.to_string(),
            path: path.clone(),
        })
        .unwrap();
        assert_eq!(a, b, "wire format must be identical to std String");
        let back = serde_json::from_str::<A>(&a).unwrap();
        assert_eq!(back.name.as_str(), name);
        assert_eq!(back.path.as_str(), path);
    }
}

#[test]
fn serde_yaml_parity_with_std_string() {
    #[derive(serde::Serialize, serde::Deserialize)]
    struct A {
        key: TriStr,
    }
    #[derive(serde::Serialize, serde::Deserialize)]
    struct B {
        key: String,
    }
    for s in ["mixed-port", &long_string()] {
        let a = serde_yaml_ng::to_string(&A {
            key: TriStr::from(s),
        })
        .unwrap();
        let b = serde_yaml_ng::to_string(&B { key: s.to_string() }).unwrap();
        assert_eq!(a, b);
        assert_eq!(serde_yaml_ng::from_str::<A>(&a).unwrap().key.as_str(), s);
    }
}

#[cfg(feature = "specta")]
#[test]
fn specta_renders_exactly_as_string() {
    use smol_str::SmolStr;

    let mut t = specta::Types::default();
    let ours = <TriStr as specta::Type>::definition(&mut t);
    assert_eq!(format!("{ours:?}"), "Primitive(str)");

    // Same shape as the native `str` and `smol_str` impls: plain `string`
    // in TS bindings, no named wrapper.
    let mut ts = specta::Types::default();
    let s = <str as specta::Type>::definition(&mut ts);
    assert_eq!(format!("{ours:?}"), format!("{s:?}"));

    let mut tsm = specta::Types::default();
    let smol = <SmolStr as specta::Type>::definition(&mut tsm);
    // SmolStr/String register a named type wrapping the same string primitive;
    // ours is the primitive itself — identical `string` in generated bindings.
    assert!(format!("{smol:?}").contains("Primitive(str)"));
}

#[test]
#[cfg_attr(miri, ignore)] // 200k-iteration timing loop; logic covered elsewhere
fn indicative_bench_clone_vs_string() {
    let n = 200_000;
    for (label, size) in [("10B", 10), ("192B", 192), ("1KiB", 1024), ("4KiB", 4096)] {
        let std_str = "x".repeat(size);
        let t = Instant::now();
        for _ in 0..n {
            black_box(std_str.clone());
        }
        let std_dur = t.elapsed();

        let v = TriStr::from(std_str.as_str());
        let t = Instant::now();
        for _ in 0..n {
            black_box(v.clone());
        }
        let tristr_dur = t.elapsed();

        println!(
            "clone x{n} ({label}):  String={std_dur:?}  TriStr={tristr_dur:?}  ({:.1}x)",
            std_dur.as_secs_f64() / tristr_dur.as_secs_f64().max(1e-9)
        );
    }
}
