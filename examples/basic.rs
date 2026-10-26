//! Basic usage examples for `tristr`.

fn main() {
    // Short strings are inline — zero heap allocation
    let key = tristr::TriStr::from("user-name");
    assert!(key.is_inline());

    // Long strings are shared — O(1) clone
    let url = tristr::TriStr::from("https://example.com/very/long/path");
    let snapshot = url.clone(); // refcount bump, no heap copy
    assert_eq!(url.shared_count(), Some(2));

    // In-place mutation when uniquely owned
    let mut mode = tristr::TriStr::from("Global");
    mode.make_ascii_lowercase();
    assert_eq!(mode, "global");

    // Zero-copy adoption from exact-capacity Strings (long enough to heap)
    let formatted = format!("https://example.com/data/item/{}", "x".repeat(40));
    let s = tristr::TriStr::from(formatted);
    assert!(!s.is_inline());

    // `write!` formatting without intermediate String allocation
    use std::fmt::Write;
    let mut log = tristr::TriStr::new();
    write!(log, "level={} msg={}", "info", "ready").unwrap();
    assert_eq!(log, "level=info msg=ready");

    // serde wire format identical to String
    #[derive(serde::Serialize, serde::Deserialize)]
    struct Config {
        name: tristr::TriStr,
    }
    let c = Config {
        name: tristr::TriStr::from("test"),
    };
    let json = serde_json::to_string(&c).unwrap();
    assert_eq!(json, r#"{"name":"test"}"#);

    println!("All examples passed!");
    println!("  inline:  {}", key);
    println!("  shared:  {} (count={:?})", url, url.shared_count());
    println!("  mutated: {}", mode);
    println!("  written: {}", log);
    drop(snapshot);
}
