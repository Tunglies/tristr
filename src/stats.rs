//! Runtime instrumentation behind the `stats` feature: length histogram at
//! construction, construction-source counters, clone counters, and COW /
//! materialization events. Zero cost when the feature is off; one relaxed
//! atomic increment per event when on. `snapshot()` prints aggregates only
//! — never string contents.

use core::fmt::Write as _;
use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

pub const BUCKETS: [&str; 8] = [
    "[0-23]  inline",
    "[24-64]",
    "[65-128]",
    "[129-240]",
    "[241-256] WINDOW",
    "[257-1K]",
    "[1K-4K]",
    "[4K+]",
];

pub static LEN: [AtomicU64; 8] = [
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
    AtomicU64::new(0),
];
pub static SRC_FROM_STR: AtomicU64 = AtomicU64::new(0);
pub static SRC_FROM_STRING: AtomicU64 = AtomicU64::new(0);
pub static SRC_WRITE: AtomicU64 = AtomicU64::new(0);
pub static SRC_FROM_ITER: AtomicU64 = AtomicU64::new(0);
pub static SRC_DESER: AtomicU64 = AtomicU64::new(0);
pub static CLONE_INLINE: AtomicU64 = AtomicU64::new(0);
pub static CLONE_HEAP: AtomicU64 = AtomicU64::new(0);
pub static COW_EVENTS: AtomicU64 = AtomicU64::new(0);
pub static PUSH_MATERIALIZATIONS: AtomicU64 = AtomicU64::new(0);

pub(crate) fn note_len(len: usize) {
    LEN[bucket_idx(len)].fetch_add(1, Relaxed);
}

fn bucket_idx(len: usize) -> usize {
    match len {
        0..=23 => 0,
        24..=64 => 1,
        65..=128 => 2,
        129..=240 => 3,
        241..=256 => 4,
        257..=1024 => 5,
        1025..=4096 => 6,
        _ => 7,
    }
}

pub fn reset() {
    for a in LEN.iter() {
        a.store(0, Relaxed);
    }
    for a in [
        &SRC_FROM_STR,
        &SRC_FROM_STRING,
        &SRC_WRITE,
        &SRC_FROM_ITER,
        &SRC_DESER,
        &CLONE_INLINE,
        &CLONE_HEAP,
        &COW_EVENTS,
        &PUSH_MATERIALIZATIONS,
    ] {
        a.store(0, Relaxed);
    }
}

pub fn snapshot() -> alloc::string::String {
    let lens: [u64; 8] = core::array::from_fn(|i| LEN[i].load(Relaxed));
    let total: u64 = lens.iter().sum();
    let pct = |n: u64| {
        if total == 0 {
            0.0
        } else {
            100.0 * n as f64 / total as f64
        }
    };
    let ld = |a: &AtomicU64| a.load(Relaxed);

    let clones = ld(&CLONE_INLINE) + ld(&CLONE_HEAP);

    let mut s = alloc::string::String::new();
    let _ = writeln!(s, "== tristr stats (aggregates only, no contents) ==");
    let _ = writeln!(s, "length histogram at construction (total {total}):");
    for (i, name) in BUCKETS.iter().enumerate() {
        let marker = if i == 4 { "  <-- window" } else { "" };
        let _ = writeln!(
            s,
            "  {name:<18} {:>8}  {:>5.1}%{marker}",
            lens[i],
            pct(lens[i])
        );
    }
    let _ = writeln!(
        s,
        "construction entry points (routes into from_&str/from_String):"
    );
    let _ = writeln!(s, "  from_&str       {:>8}", ld(&SRC_FROM_STR));
    let _ = writeln!(s, "  from_String     {:>8}", ld(&SRC_FROM_STRING));
    let _ = writeln!(
        s,
        "  deser overlay   {:>8}   write/push {:>8}   from_iter {:>8}",
        ld(&SRC_DESER),
        ld(&SRC_WRITE),
        ld(&SRC_FROM_ITER)
    );
    let _ = writeln!(
        s,
        "clones: inline {:>8} / heap {:>8}  ({:.2} per construction)",
        ld(&CLONE_INLINE),
        ld(&CLONE_HEAP),
        if total == 0 {
            0.0
        } else {
            clones as f64 / total as f64
        }
    );
    let _ = writeln!(
        s,
        "COW events: {}   push materializations: {}",
        ld(&COW_EVENTS),
        ld(&PUSH_MATERIALIZATIONS)
    );

    s
}

#[cfg(test)]
mod tests {
    // Other tests in this binary construct/clone concurrently; assert deltas
    // on counters only this test exercises, and >= for shared ones.
    #[test]
    fn counters_advance() {
        use core::sync::atomic::Ordering::Relaxed;
        let (b4, bh) = (super::LEN[4].load(Relaxed), super::CLONE_HEAP.load(Relaxed));
        let _ = crate::TriStr::from("x".repeat(250).as_str()); // window bucket
        let c = crate::TriStr::from("x".repeat(64).as_str());
        let _ = c.clone();
        let snap = super::snapshot();
        assert!(snap.contains("WINDOW"));
        assert_eq!(super::LEN[4].load(Relaxed), b4 + 1);
        assert!(super::CLONE_HEAP.load(Relaxed) >= bh + 1);
    }
}
