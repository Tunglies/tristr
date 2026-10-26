# Changelog

## 0.1.0

- 24-byte three-state string: inline (23-byte payload), owned (`Box<str>`,
  zero-copy from exact-capacity `String`s), shared (`Arc<str>` via a raw
  data pointer — branch-free `Deref`).
- O(1) `Clone` for shared values; owned values deep-copy once, the copy is
  shared.
- Allocator-aware deserialization: long strings route to the shared path
  when `len + 16 > 1024`, otherwise owned.
- NEON-optimized short-string operations: `make_ascii_uppercase/lowercase`
  (single 16B vector channel), `PartialEq`/`Ord`/`PartialOrd::lt` (~0.2 ns
  tag-direct compare), `fmt::Write`/`push_str` (register-pair fragment
  copy), `sort` (prefix compare without `memcmp` call).
- Split DerefMut fast path: uniqueness check inlined, COW materialization
  isolated to a cold out-of-line function.
- `From<String>` (zero-copy for exact capacity), `From<Cow>`, `From<&String>`,
  `FromStr`, `AsRef<[u8]>`, `PartialEq<String>` (bidirectional),
  `capacity()`/`shrink_to_fit()`.
- serde: wire format byte-identical to `String`. specta feature: native
  `Type` emitting `string`.
- `no_std(alloc)` support (`std` feature, default on).
- `stats` feature: construction/clone/COW counters, zero cost when off.
- `alias::String` for drop-in migration from `smartstring::alias::String`.
