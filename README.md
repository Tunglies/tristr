# tristr

A 24-byte string: 23 bytes inline, shared `Arc` copy-on-write beyond that,
owned `Box` adoption for exact-capacity `String` conversions.

`size_of::<tristr::TriStr>() == size_of::<String>() == 24` on 64-bit targets.

## Why

`tristr` combines properties no other inline-string crate offers simultaneously:

| property | tristr | String | compact_str | smol_str | arcstr | smartstring (archived) |
| --- | --- | --- | --- | --- | --- | --- |
| `size_of` (64-bit) | 24 | 24 | 24 | 24 | **8** | 24 |
| inline capacity | 23 | 0 | **24** | 23 | 0 | 23 |
| `Clone` long strings | **O(1)**† | O(n) | O(n) | **O(1)** | **O(1)** | O(n) |
| in-place mutation (`DerefMut`) | **yes**‡ | yes | yes | no | no | yes |
| `From<String>` zero-copy | **yes** | (identity) | yes | no | no | no |
| `fmt::Write` (`write!`) | **yes** | yes | yes | no | no | yes |
| serde wire format | = `String` | baseline | = | = | = | = |
| native `specta::Type` → `string` | **yes** | yes | no | yes | no | no |
| `no_std(alloc)` | **yes** | — | no | no | no | yes |

† Owned-adoption values deep-copy once; clones thereafter are O(1) shared.
‡ COW when shared, in place when owned or uniquely held.

## Usage

```rust
use tristr::TriStr;

// Short strings (≤23 bytes) are stored inline — zero heap allocation.
let key = TriStr::from("user-name");

// Clone a long string: O(1) refcount bump, not a heap copy.
let url = TriStr::from("https://example.com/very/long/path/to/resource");
let snapshot = url.clone();

// Mutate in place when uniquely owned; copy-on-write when shared.
let mut mode = TriStr::from("Global");
mode.make_ascii_lowercase();
assert_eq!(mode, "global");

// Zero-copy adoption from exact-capacity Strings.
let formatted = format!("session-{}", "4f2a");
let s = TriStr::from(formatted); // no extra allocation

// `write!` formatting without intermediate String allocation.
use std::fmt::Write;
let mut log = TriStr::new();
write!(log, "level={} msg={}", "info", "ready").unwrap();
```

Drop-in replacement for `smartstring::alias::String`:

```rust
use tristr::alias::String;
// rest of your code unchanged
```

With `stats` feature enabled:

```rust
let _ = TriStr::from("test");
println!("{}", tristr::stats::snapshot());
// == tristr stats (aggregates only, no contents) ==
// length histogram at construction (total 1):
//   [0-23]  inline         1  100.0%
//   ...
```

## Features

| feature | default | description |
| --- | --- | --- |
| `std` | ✓ | Link against the Rust standard library. Required by `specta`. |
| `serde` | ✓ | `Serialize` / `Deserialize` with wire format byte-identical to `String` (JSON, YAML, etc.). |
| `specta` | | Native `specta::Type` emitting `string` — TS bindings with zero annotations. Requires `std`. |
| `stats` | | Runtime counters: length histogram, construction sources, clone/COW events. Zero cost when off. |

```toml
# Default: std + serde
tristr = "0.1"

# Everything (specta for tauri/tauri-specta)
tristr = { version = "0.1", features = ["specta"] }

# no_std (embedded / kernel)
tristr = { version = "0.1", default-features = false, features = ["serde"] }

# Profiling instrumentation
tristr = { version = "0.1", features = ["stats"] }
```

## Design

Three `#[repr(C)]` union variants share a tag byte at offset 23:

- **inline**: `[u8; 23]` payload + tag `0x80 | len` — no heap for ≤ 23 bytes.
- **owned**: `Box<str>` (16-byte fat pointer) — adopted zero-copy from
  exact-capacity `String`s and from deserialization of long strings.
- **shared**: `Arc<str>` stored as a raw data pointer — O(1) clone with a
  branch-free `Deref` (the fat-pointer shape matches `Box<str>`, enabling
  LLVM if-conversion across variants).

`DerefMut` mutates inline strings in place; owned values mutate in place
(unique ownership); shared values mutate through `Arc::get_mut` when
uniquely held and copy-on-write when shared.

Short-string operations (`make_ascii_uppercase/lowercase`, `PartialEq`,
`Ord`, `fmt::Write`, sort compare) use single-channel NEON/SIMD
instructions for sub-nanosecond performance.

Construction, hashing, ordering and serialization all delegate to `str`, so
`tristr::TriStr` is wire- and semantics-compatible with `std::string::String`
everywhere it matters (JSON, YAML, `HashMap` keys, TS bindings).

## Memory characteristics

Measured against `std::string::String` on macOS (200k live strings, peak RSS,
system allocator):

| payload length | tristr vs `String` RSS |
| --- | --- |
| ≤ 240 B | parity (header absorbed by allocator quantum) |
| 241 – 256 B | +62 B/string (the only window) |
| ≥ 257 B | parity (`String` crosses the same cliff) |

Clone-heavy workloads (1KB strings cloned 100×): **18× less memory** than `String`.

Allocation profile — the only type achieving all three:
- Short construction: **0 allocations**
- Long-string clone: **0 allocations**  
- In-place mutation: **0 allocations**

## Safety

The heap representation delegates all synchronization to std's `Arc<str>` —
this crate contains no custom atomics, CAS protocols, or refcount code.
The remaining `unsafe` is limited to union tag reads (invariant-driven,
compile-time asserted) and `ManuallyDrop` release behind the tag check.

Compile-time layout assertions pin `offset_of!(Inline, tag) == 23`,
`size_of::<Inline>() == 24`. The test suite covers construction, COW sharing,
formatting, serde parity with `String` (JSON + YAML), map-key semantics,
specta output and auto traits; a deterministic deep-path suite runs under
Miri (single-seed and `-Zmiri-many-seeds`); `fuzz/` holds 7 libFuzzer targets
with 100M+ cumulative executions.

## License

Apache-2.0
