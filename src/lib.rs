//! 24-byte inline (23B payload) + `Arc`-backed copy-on-write string (`TriStr`).
//!
//! Heap layout is std's `Arc<str>`: sharing, drop and unique-owner mutation
//! are std-audited (`Arc::into_raw`/`from_raw` are the only handle
//! operations); this crate owns no synchronization code. The shared variant
//! stores the Arc's *data* pointer so both heap variants read
//! (data pointer, length) from offset 0.
//!
//! Both union variants are `#[repr(C)]` with a shared tag byte at offset 23,
//! read via the `inline.tag` union field (u8 has no validity constraints;
//! pointer-arithmetic discriminant reads measurably regress hot-path
//! codegen); the register-built inline construction from `&str`;
//! bounds-check-free raw-parts slice building; in-register (call-free)
//! fragment copies for inline appends and ≤16-byte prefix comparisons; and
//! the `#[cold]` COW and heap-clone slow paths.

#![cfg_attr(not(feature = "std"), no_std)]

use alloc::sync::Arc;
use core::fmt;
use core::hash::{Hash, Hasher};
use core::mem::{ManuallyDrop, MaybeUninit};
use core::ops::{Deref, DerefMut};
use core::ptr;

const INLINE_CAP: usize = 23;
const TAG_OFFSET: usize = 23;
const INLINE_FLAG: u8 = 0x80;
const INLINE_LEN: u8 = 0x7f;
/// Tag byte marking the owned (`Box<str>`) variant.
const BOXED_TAG: u8 = 0x01;

/// Inline payload. Carrying the bytes inside a union with a unit variant
/// makes the tail bytes ([len..23)) uninit-tolerant for validity purposes —
/// exactly like struct padding — so construction may leave them
/// uninitialized. The bytes variant is only ever read at [0..len).
#[derive(Clone, Copy)]
union InlineData {
    bytes: [u8; INLINE_CAP],
    _void: (),
}

#[derive(Clone, Copy)]
#[repr(C)]
struct Inline {
    data: InlineData,
    tag: u8, // INLINE_FLAG | len
}

/// Heap variant (shared): the `Arc<str>`'s *data* pointer (from
/// `Arc::into_raw`) as a fat `*const str` plus 7 bytes of padding that are
/// never read or written, and a tag byte at offset 23 (always 0, encoding
/// "heap"). Storing the data pointer instead of the `Arc` handle puts
/// (data pointer, length) at offset 0 for both heap variants, so `deref`
/// reads them with one `ldp` and no variant arithmetic — `Box<str>` and
/// `Arc::into_raw` products have identical fat-pointer shape. The `Arc`
/// handle is reconstructed (exactly once per operation) via
/// `Arc::from_raw` for clone, drop and refcount queries; the allocation,
/// counters and drop protocol remain std `Arc<str>`'s. Leaving the padding
/// uninitialized saves a zero store per heap construction; moving the value
/// copies uninitialized bytes, which is permitted (exactly like struct
/// padding).
#[repr(C)]
struct Long {
    raw: *const str,
    pad: [MaybeUninit<u8>; 7],
    tag: u8,
}

// SAFETY: `raw` uniquely owns one `Arc<str>` reference obtained from
// `Arc::into_raw` (see `from_arc`); moving `Long` moves that reference.
// `Arc<str>` is `Send + Sync`, so the raw-pointer field is too.
unsafe impl Send for Long {}
// SAFETY: `&Long` grants no mutation and `Arc<str>`'s data is immutable
// while shared; `Sync` for `Arc<str>` therefore carries over.
unsafe impl Sync for Long {}

const _: () = {
    // Compile-time layout invariants the tag read and the [u64; 3]
    // transmute in `From<&str>` rely on.
    assert!(core::mem::offset_of!(Inline, tag) == TAG_OFFSET);
    assert!(core::mem::size_of::<Inline>() == 24);
    assert!(core::mem::size_of::<Long>() == 24);
    assert!(core::mem::offset_of!(Long, tag) == TAG_OFFSET);
    assert!(core::mem::size_of::<Boxed>() == 24);
    assert!(core::mem::offset_of!(Boxed, tag) == TAG_OFFSET);
    assert!(core::mem::size_of::<Repr>() == 24);
    assert!(core::mem::align_of::<Repr>() == 8);
};

/// Owned variant: a `Box<str>` adopted zero-copy from an exact-capacity
/// `String` via `From<String>`. Owned values mutate in place with zero
/// allocation; cloning deep-copies (the clone itself is shared and O(1) to
/// clone again). Same 16B fat-pointer shape as the `Long` variant.
#[repr(C)]
struct Boxed {
    inner: ManuallyDrop<alloc::boxed::Box<str>>,
    pad: [MaybeUninit<u8>; 7],
    tag: u8, // BOXED_TAG
}

union Repr {
    inline: Inline,
    long: ManuallyDrop<Long>,   // tag 0: shared Arc<str>
    boxed: ManuallyDrop<Boxed>, // tag 1: owned Box<str>
}

/// Copy a ≤23-byte fragment (`push_str`'s inline append) without a libc
/// `memcpy` call: head/tail load-store pairs whose overlapping tail keeps
/// every access inside [0, n). Register-width and count follow the same
/// shape as the register-built `From<&str>` construction.
///
/// # Safety
/// `n` must be ≤ 24; `[src, src+n)` and `[dst, dst+n)` must be valid and
/// non-overlapping.
#[inline(always)]
unsafe fn copy_inline_fragment(dst: *mut u8, src: *const u8, n: usize) {
    use core::ptr::{read_unaligned as load, write_unaligned as store};
    debug_assert!(n <= 24);
    // SAFETY: every load/store pair addresses only bytes of [0, n): the
    // head pair covers [0, h) and the tail pair [n-t, n) for widths
    // h = t ∈ {8, 4, 2}; for n < 2 only the single byte is touched. For
    // n ≥ 16 the 16-byte head plus the 8-byte tail cover [0, n) because
    // n ≤ 24 implies n - 8 ≤ 16.
    unsafe {
        if n >= 16 {
            store(dst as *mut u64, load(src as *const u64));
            store(dst.add(8) as *mut u64, load(src.add(8) as *const u64));
            store(
                dst.add(n - 8) as *mut u64,
                load(src.add(n - 8) as *const u64),
            );
        } else if n >= 8 {
            store(dst as *mut u64, load(src as *const u64));
            store(
                dst.add(n - 8) as *mut u64,
                load(src.add(n - 8) as *const u64),
            );
        } else if n >= 4 {
            store(dst as *mut u32, load(src as *const u32));
            store(
                dst.add(n - 4) as *mut u32,
                load(src.add(n - 4) as *const u32),
            );
        } else if n >= 2 {
            store(dst as *mut u16, load(src as *const u16));
            store(
                dst.add(n - 2) as *mut u16,
                load(src.add(n - 2) as *const u16),
            );
        } else if n == 1 {
            *dst = *src;
        }
    }
}

/// In-place ASCII case fold of the inline payload, covering [0,16) with
/// one 16-byte vector pass and [16,24) with one 8-byte pass when the
/// string is longer than 16 bytes. Accesses never leave the 24-byte
/// value: bytes at [len,16) and [len,23) are uninitialized-tolerant
/// payload (same status as struct padding — moves already copy them), so
/// folding them writes lane-poison back over lane-poison, which no reader
/// observes (every read path touches [0, len) or copies all 24 bytes).
/// The 8-byte pass re-stores the tag byte at offset 23, which folds to
/// itself: inline tags are `0x80 | len ∈ [0x80, 0x97]`, disjoint from
/// both ASCII-letter ranges — checked at compile time below.
///
/// One 16-byte pass beats two 8-byte passes measurably (~1.3 vs ~2.8 ns
/// for a 10-byte string): a single load/op/store chain with a
/// constant-offset address, no overlapping stores and no runtime-length
/// address arithmetic feeding the tail load.
///
/// # Safety
/// `p` must point at a `TriStr` whose tag byte marks the inline variant,
/// and `n` must be that value's length (≤ 23).
#[inline(always)]
unsafe fn set_ascii_case<const UPPER: bool>(p: *mut u8, n: usize) {
    debug_assert!(n <= INLINE_CAP);
    let base: u8 = if UPPER { b'a' } else { b'A' };
    #[cfg(all(target_arch = "aarch64", not(miri)))]
    // SAFETY: see the doc comment — both accesses stay inside the 24-byte
    // value, and every byte they write beyond the string is either
    // uninit-tolerant payload or the tag, which folds to itself.
    unsafe {
        use core::arch::aarch64::*;
        let v = vld1q_u8(p);
        let d = vsubq_u8(v, vdupq_n_u8(base));
        let m = vandq_u8(vcltq_u8(d, vdupq_n_u8(26)), vdupq_n_u8(0x20));
        vst1q_u8(p, veorq_u8(v, m));
        if n > 16 {
            let t = p.add(16);
            let v = vld1_u8(t);
            let d = vsub_u8(v, vdup_n_u8(base));
            let m = vand_u8(vclt_u8(d, vdup_n_u8(26)), vdup_n_u8(0x20));
            vst1_u8(t, veor_u8(v, m));
        }
    }
    #[cfg(any(not(target_arch = "aarch64"), miri))]
    // SAFETY: the fallback folds exactly [0, n) byte by byte.
    unsafe {
        for i in 0..n {
            let b = *p.add(i);
            if b.wrapping_sub(base) < 26 {
                *p.add(i) = b ^ 0x20;
            }
        }
    }
}

const _: () = {
    // The [16,24) fold re-stores the tag byte; it must fold to itself.
    const fn within(tag: u8, lo: u8, hi: u8) -> bool {
        tag >= lo && tag <= hi
    }
    let mut len = 0usize;
    while len <= INLINE_CAP {
        let tag = INLINE_FLAG | len as u8;
        assert!(!(within(tag, 0x61, 0x7a) || within(tag, 0x41, 0x5a)));
        len += 1;
    }
};

/// Convenient alias mirroring `smartstring::alias::String`: import this as
/// `use tristr::alias::String;` for a drop-in replacement of
/// `alloc::string::String` / `smartstring::alias::String`.
extern crate alloc;

#[cfg(feature = "stats")]
pub mod stats;

pub mod alias {
    pub type String = crate::TriStr;
}

pub struct TriStr(Repr);

impl TriStr {
    /// Mirrors `alloc::string::String::new`.
    pub const fn new() -> Self {
        TriStr(Repr {
            inline: Inline {
                data: InlineData { _void: () },
                tag: INLINE_FLAG,
            },
        })
    }

    fn from_arc(arc: Arc<str>) -> Self {
        TriStr(Repr {
            long: ManuallyDrop::new(Long {
                raw: Arc::into_raw(arc),
                pad: [MaybeUninit::uninit(); 7],
                tag: 0,
            }),
        })
    }

    fn from_short_string(s: alloc::string::String) -> Self {
        TriStr::from(s.as_str())
    }

    /// Slack-capacity `String` adoption: `into_boxed_str` would
    /// shrink-realloc, so the value copies into a shared Arc allocation.
    /// Outlined (`#[inline(never)]`) because its `Arc::from` body carries
    /// five live registers across the allocator call; keeping it out of
    /// `From<String>` lets that constructor inline into callers with a
    /// two-pair frame instead of four.
    #[inline(never)]
    fn from_slack_string(s: alloc::string::String) -> Self {
        TriStr::from_arc(Arc::from(s))
    }

    fn from_boxed(b: alloc::boxed::Box<str>) -> Self {
        TriStr(Repr {
            boxed: ManuallyDrop::new(Boxed {
                inner: ManuallyDrop::new(b),
                pad: [MaybeUninit::uninit(); 7],
                tag: BOXED_TAG,
            }),
        })
    }

    #[inline(always)]
    fn tag(&self) -> u8 {
        // SAFETY: every construction path writes the tag byte; reading the
        // u8 union field directly is sound because u8 has no validity
        // constraints, and the byte is initialized regardless of the active
        // variant (the payload tail may be uninitialized but is never read
        // through this field).
        unsafe { self.0.inline.tag }
    }

    #[inline(always)]
    pub fn is_inline(&self) -> bool {
        self.tag() & INLINE_FLAG != 0
    }

    /// True when this value owns its buffer outright (adopted zero-copy
    /// from an exact-capacity `String`).
    #[inline(always)]
    pub fn is_boxed(&self) -> bool {
        self.tag() == BOXED_TAG
    }

    /// Debug/introspection: Arc strong count when shared, `None` when
    /// inline or owned.
    pub fn shared_count(&self) -> Option<usize> {
        if self.is_inline() || self.is_boxed() {
            None
        } else {
            // SAFETY: tag says the `long` variant is active; the handle is
            // reconstructed and forgotten so the count is untouched.
            let arc = unsafe { Arc::from_raw(self.0.long.raw) };
            let n = Arc::strong_count(&arc);
            core::mem::forget(arc);
            Some(n)
        }
    }

    /// Mirrors `smartstring`'s `capacity()`: inline values report the inline
    /// capacity (23); heap values report the Arc allocation's usable length
    /// (Arc buffers are exact-sized — there is no reserved slack).
    pub fn capacity(&self) -> usize {
        if self.is_inline() {
            INLINE_CAP
        } else {
            self.as_str().len()
        }
    }

    /// Mirrors `smartstring`'s `shrink_to_fit()`: a no-op. Inline values are
    /// fixed-size; heap values live in exact-sized shared allocations that
    /// are never over-reserved.
    pub fn shrink_to_fit(&mut self) {}

    #[inline(always)]
    pub fn as_str(&self) -> &str {
        self
    }
}

impl Deref for TriStr {
    type Target = str;

    #[inline(always)]
    fn deref(&self) -> &str {
        // Read the tag exactly once: it both discriminates the variant and
        // carries the inline length.
        let tag = self.tag();
        // SAFETY: variant checks via tag; inline payload was copied from a
        // valid `&str`, so the first (tag & INLINE_LEN) bytes are valid UTF-8
        // and form the complete string. Building the slice from raw parts
        // skips a redundant bounds check: tag & INLINE_LEN <= 23 by
        // construction. Both heap variants keep (data pointer, length) at
        // offset 0 — `Long` stores the `Arc::into_raw` product and `Boxed`
        // the `Box<str>` — and reading through `long` while `boxed` is
        // active is sound: a `*const str` only requires a non-null
        // 1-aligned pointer and usize metadata, which a `Box<str>` fat
        // pointer provides, and the referent bytes are valid UTF-8 in both
        // variants. Heap payloads are written before becoming reachable and
        // immutable while shared.
        unsafe {
            if tag & INLINE_FLAG != 0 {
                let len = (tag & INLINE_LEN) as usize;
                let data = ptr::addr_of!(self.0.inline.data.bytes) as *const u8;
                str::from_utf8_unchecked(core::slice::from_raw_parts(data, len))
            } else {
                &*self.0.long.raw
            }
        }
    }
}

impl DerefMut for TriStr {
    #[inline(always)]
    fn deref_mut(&mut self) -> &mut str {
        let tag = self.tag();
        // SAFETY: same variant and length reasoning as `deref`. Owned
        // (`boxed`) values mutate in place — unique ownership by
        // construction; shared heap strings go through the COW slow path;
        // inline strings are mutated in place.
        unsafe {
            if tag & INLINE_FLAG != 0 {
                let len = (tag & INLINE_LEN) as usize;
                let data = ptr::addr_of_mut!(self.0.inline.data.bytes) as *mut u8;
                str::from_utf8_unchecked_mut(core::slice::from_raw_parts_mut(data, len))
            } else if tag == BOXED_TAG {
                let b = &mut (*self.0.boxed).inner;
                &mut *b
            } else {
                // The uniqueness check stays inline — a single load — so
                // uniquely owned heap values skip the call frame of the COW
                // path below, which is sized for the copy's register
                // pressure (a dozen stack instructions per call).
                // `Arc::get_mut` is deliberately not used: its uniqueness
                // protocol (std 1.99) CAS-locks the weak counter to rule
                // out a concurrent `Weak::upgrade` — an entire RMW sequence
                // for a race that cannot occur here, because the Arc handles
                // TriStr stores never escape the crate, so no `Weak` can
                // reference the allocation. With no Weak possible, strong ==
                // 1 under our exclusive borrow is exactly the uniqueness
                // `Arc::get_mut` would establish.
                let arc = Arc::from_raw(self.0.long.raw);
                if Arc::strong_count(&arc) == 1 {
                    core::mem::forget(arc);
                    // SAFETY: ours is the only strong reference and self
                    // owns it for the duration of this borrow, so mutation
                    // through the data pointer is sound.
                    &mut *(self.0.long.raw as *mut str)
                } else {
                    core::mem::forget(arc);
                    self.deref_mut_cow()
                }
            }
        }
    }
}

impl Drop for TriStr {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: the tag picks exactly one owning variant; releasing the
        // owning handle exactly once behind that check.
        unsafe {
            let tag = self.tag();
            if tag & INLINE_FLAG == 0 {
                if tag == BOXED_TAG {
                    ManuallyDrop::drop(&mut (*self.0.boxed).inner);
                } else {
                    drop(Arc::from_raw(self.0.long.raw));
                }
            }
        }
    }
}

impl Clone for TriStr {
    #[inline]
    fn clone(&self) -> Self {
        // SAFETY: inline repr owns no resource, so a bitwise copy is a valid
        // value whose Drop sees tag == inline. Heap reprs take the
        // out-of-line slow path.
        unsafe {
            if self.is_inline() {
                #[cfg(feature = "stats")]
                crate::stats::CLONE_INLINE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                ptr::read(self)
            } else {
                self.clone_slow()
            }
        }
    }
}

impl TriStr {
    /// Heap clone slow path: owned (`boxed`) values deep-copy — the copy
    /// itself is shared and O(1) to clone again; shared (`long`) values
    /// bump the Arc refcount. Kept cold and out-of-line so `clone`'s
    /// inline case stays small enough to inline into callers (measured:
    /// with the paths merged, `clone` exceeds the inline budget and every
    /// clone of a short string pays a call frame).
    #[cold]
    #[inline(never)]
    fn clone_slow(&self) -> Self {
        #[cfg(feature = "stats")]
        crate::stats::CLONE_HEAP.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        // SAFETY: the caller checked the tag: a heap variant is active.
        unsafe {
            if self.is_boxed() {
                TriStr::from(self.as_str())
            } else {
                let arc = Arc::from_raw(self.0.long.raw);
                let copy = TriStr::from_arc(Arc::clone(&arc));
                core::mem::forget(arc);
                copy
            }
        }
    }
}

impl Default for TriStr {
    fn default() -> Self {
        TriStr::new()
    }
}

impl<'a> From<alloc::borrow::Cow<'a, str>> for TriStr {
    fn from(s: alloc::borrow::Cow<'a, str>) -> Self {
        match s {
            alloc::borrow::Cow::Borrowed(s) => TriStr::from(s),
            alloc::borrow::Cow::Owned(s) => TriStr::from(s),
        }
    }
}

impl From<&alloc::string::String> for TriStr {
    fn from(s: &alloc::string::String) -> Self {
        TriStr::from(s.as_str())
    }
}

impl AsRef<[u8]> for TriStr {
    fn as_ref(&self) -> &[u8] {
        self.as_str().as_bytes()
    }
}

impl core::str::FromStr for TriStr {
    type Err = core::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(TriStr::from(s))
    }
}

impl FromIterator<alloc::string::String> for TriStr {
    fn from_iter<I: IntoIterator<Item = alloc::string::String>>(iter: I) -> Self {
        #[cfg(feature = "stats")]
        crate::stats::SRC_FROM_ITER.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let mut iter = iter.into_iter();
        match (iter.next(), iter.next()) {
            (None, _) => TriStr::new(),
            (Some(s), None) => TriStr::from(s),
            (Some(mut s), Some(second)) => {
                s.push_str(&second);
                for rest in iter {
                    s.push_str(&rest);
                }
                TriStr::from(s)
            }
        }
    }
}

impl TriStr {
    /// Append `s` to the end (see TriStr::push_str for growth semantics:
    /// inline appends are free; the shared heap allocation is immutable, so
    /// growth materializes a new value in O(len)).
    ///
    /// The inline fast path reads the tag exactly once — the current length
    /// comes from the tag itself instead of a full `deref` — and copies the
    /// fragment with in-register load/store pairs: a variable-size
    /// `copy_nonoverlapping` lowers to a libc `memcpy` call, whose
    /// call/return overhead dwarfs a ≤23-byte copy (three `write!` fragments
    /// per formatted value pay it three times).
    pub fn push_str(&mut self, s: &str) {
        #[cfg(feature = "stats")]
        crate::stats::SRC_WRITE.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let add = s.len();
        let tag = self.tag();
        if tag & INLINE_FLAG != 0 {
            let cur = (tag & INLINE_LEN) as usize;
            let new = cur + add;
            if new <= INLINE_CAP {
                // SAFETY: the tag says inline, so [0, cur) bytes of the
                // payload are initialized; [cur, cur+add) stays within the
                // 23-byte payload; `s` is external and disjoint; the tag is
                // updated after the copy under an exclusive borrow.
                unsafe {
                    let dst = (ptr::addr_of_mut!(self.0.inline.data.bytes) as *mut u8).add(cur);
                    copy_inline_fragment(dst, s.as_ptr(), add);
                    self.0.inline.tag = INLINE_FLAG | new as u8;
                }
                return;
            }
        }
        self.push_str_slow(s);
    }

    /// Materialization slow path of `push_str` (heap value, or inline
    /// overflow). Outlined so the inline fast path stays small enough to
    /// inline into `write_str` callers.
    #[cold]
    #[inline(never)]
    fn push_str_slow(&mut self, s: &str) {
        #[cfg(feature = "stats")]
        crate::stats::PUSH_MATERIALIZATIONS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let cur = self.as_str().len();
        let add = s.len();
        let mut buf = alloc::string::String::with_capacity(cur + add);
        buf.push_str(self.as_str());
        buf.push_str(s);
        *self = TriStr::from(buf);
    }

    /// COW slow path of `deref_mut`: the value is shared, so mutation
    /// materializes a private copy. Kept cold and out-of-line — the inline
    /// uniqueness check in `deref_mut` already routed uniquely owned values
    /// past this function, and its shared frame (sized for the copy's
    /// allocator/memcpy register pressure) would otherwise tax every
    /// caller.
    #[cold]
    #[inline(never)]
    fn deref_mut_cow(&mut self) -> &mut str {
        // SAFETY: the caller checked the tag and the strong count: the
        // `long` variant is active and shared. The long variant implies
        // len >= INLINE_CAP + 1, so the copy always takes the Arc path;
        // spelling it out keeps the generic `From<&str>` (and its dead
        // inline branch) out of this function's frame.
        let copy = TriStr::from_arc(Arc::from(self.as_str()));
        #[cfg(feature = "stats")]
        crate::stats::COW_EVENTS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        *self = copy; // drops our shared handle, installs an owned one
        self.deref_mut()
    }

    /// Append a single character.
    pub fn push(&mut self, ch: char) {
        self.push_str(ch.encode_utf8(&mut [0u8; 4]));
    }

    /// Converts the value to ASCII uppercase in place, mirroring
    /// `str::make_ascii_uppercase` byte for byte. Inherent on `TriStr`, so
    /// method resolution picks it over the `str` method reached through
    /// `DerefMut`: inline values (the common short-string case) fold
    /// [0, len) in registers via `set_ascii_case` instead of the generic
    /// variable-length slice loop; heap values keep `str`'s vectorized
    /// loop (after the usual COW check) through `deref_mut`.
    pub fn make_ascii_uppercase(&mut self) {
        let tag = self.tag();
        if tag & INLINE_FLAG != 0 {
            let len = (tag & INLINE_LEN) as usize;
            // SAFETY: the tag says inline, so [0, len) of the payload is
            // initialized; `set_ascii_case` accesses only [0, len).
            unsafe {
                let p = ptr::addr_of_mut!(self.0.inline.data.bytes) as *mut u8;
                set_ascii_case::<true>(p, len);
            }
        } else {
            DerefMut::deref_mut(self).make_ascii_uppercase();
        }
    }

    /// Converts the value to ASCII lowercase in place, mirroring
    /// `str::make_ascii_lowercase` byte for byte (see
    /// [`make_ascii_uppercase`](Self::make_ascii_uppercase) for the
    /// inline fast path).
    pub fn make_ascii_lowercase(&mut self) {
        let tag = self.tag();
        if tag & INLINE_FLAG != 0 {
            let len = (tag & INLINE_LEN) as usize;
            // SAFETY: same as `make_ascii_uppercase`.
            unsafe {
                let p = ptr::addr_of_mut!(self.0.inline.data.bytes) as *mut u8;
                set_ascii_case::<false>(p, len);
            }
        } else {
            DerefMut::deref_mut(self).make_ascii_lowercase();
        }
    }
}

impl fmt::Write for TriStr {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.push_str(s);
        Ok(())
    }
}

impl core::ops::Add<&str> for TriStr {
    type Output = TriStr;
    fn add(mut self, rhs: &str) -> TriStr {
        self.push_str(rhs);
        self
    }
}

impl core::ops::AddAssign<&str> for TriStr {
    fn add_assign(&mut self, rhs: &str) {
        self.push_str(rhs);
    }
}

impl<'a> FromIterator<&'a str> for TriStr {
    fn from_iter<I: IntoIterator<Item = &'a str>>(iter: I) -> Self {
        #[cfg(feature = "stats")]
        crate::stats::SRC_FROM_ITER.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let mut buf = alloc::string::String::new();
        for s in iter {
            buf.push_str(s);
        }
        TriStr::from(buf)
    }
}

impl FromIterator<char> for TriStr {
    fn from_iter<I: IntoIterator<Item = char>>(iter: I) -> Self {
        #[cfg(feature = "stats")]
        crate::stats::SRC_FROM_ITER.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        let mut buf = alloc::string::String::new();
        for c in iter {
            buf.push(c);
        }
        TriStr::from(buf)
    }
}

impl From<&str> for TriStr {
    #[inline]
    fn from(s: &str) -> Self {
        use core::ptr::read_unaligned as load;

        let len = s.len();
        #[cfg(feature = "stats")]
        {
            crate::stats::note_len(len);
            crate::stats::SRC_FROM_STR.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
        let src = s.as_ptr();
        if len <= INLINE_CAP {
            // Build the whole 24-byte value in registers as three little-
            // endian words, then materialize it with at most three aligned
            // non-overlapping stores into the returned value. Writing an
            // `Inline` through memory instead (tag store + overlapping
            // payload stores) forces a store-to-load round trip when the
            // value is moved to its destination, which costs ~1ns on
            // Apple Silicon for a 10-byte string.
            let tag_word = ((INLINE_FLAG | len as u8) as u64) << 56;
            let (w0, w1, w2);
            // SAFETY: every load reads only bytes of `s` inside [0, len);
            // the overlapping tail load stays within [len - 8, len).
            unsafe {
                if len >= 16 {
                    w0 = load(src as *const u64);
                    w1 = load(src.add(8) as *const u64);
                    let tail = load(src.add(len - 8) as *const u64);
                    // bytes [16, len) are the top len-16 bytes of `tail`;
                    // a shift of 64 (len == 16) would overflow, so guard it.
                    w2 = if len == 16 {
                        tag_word
                    } else {
                        (tail >> ((24 - len) * 8)) | tag_word
                    };
                } else if len >= 8 {
                    w0 = load(src as *const u64);
                    let tail = load(src.add(len - 8) as *const u64);
                    // bytes [8, len) are the top len-8 bytes of `tail`.
                    w1 = if len == 8 {
                        0
                    } else {
                        tail >> ((16 - len) * 8)
                    };
                    w2 = tag_word;
                } else if len >= 4 {
                    let head = load(src as *const u32) as u64;
                    let tail = load(src.add(len - 4) as *const u32) as u64;
                    w0 = head | (tail << ((len - 4) * 8));
                    w1 = 0;
                    w2 = tag_word;
                } else if len >= 2 {
                    let head = load(src as *const u16) as u64;
                    let tail = load(src.add(len - 2) as *const u16) as u64;
                    w0 = head | (tail << ((len - 2) * 8));
                    w1 = 0;
                    w2 = tag_word;
                } else if len == 1 {
                    w0 = *src as u64;
                    w1 = 0;
                    w2 = tag_word;
                } else {
                    w0 = 0;
                    w1 = 0;
                    w2 = tag_word;
                }
            }
            // SAFETY: [u64; 3] and Repr are both 24 bytes and 8-aligned, and
            // every byte of `words` is initialized. Only the `inline` field
            // is ever read while the tag byte says inline (u8 payload and
            // tag have no validity constraints), so no inactive-variant
            // read of `long` can occur through this value.
            TriStr(unsafe { core::mem::transmute::<[u64; 3], Repr>([w0, w1, w2]) })
        } else {
            TriStr::from_arc(Arc::from(s))
        }
    }
}

impl From<alloc::string::String> for TriStr {
    // Without the hint this compiles out-of-line with a 4-pair register
    // prologue around the (already tiny) exact-capacity adoption path; with
    // LTO the `into_boxed_str` interior capacity check folds away against
    // our `capacity() == len()` test, so the whole hot path is six stores
    // and branches.
    #[inline]
    fn from(s: alloc::string::String) -> Self {
        #[cfg(feature = "stats")]
        {
            crate::stats::note_len(s.len());
            crate::stats::SRC_FROM_STRING.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
        if s.len() <= INLINE_CAP {
            TriStr::from_short_string(s)
        } else if s.capacity() == s.len() {
            // Exact-capacity: adopt the buffer with zero allocation and
            // zero copy. The value is owned; see `is_boxed` for the clone
            // semantics.
            TriStr::from_boxed(s.into_boxed_str())
        } else {
            TriStr::from_slack_string(s)
        }
    }
}

impl From<TriStr> for alloc::string::String {
    fn from(v: TriStr) -> Self {
        use alloc::borrow::ToOwned;
        if v.is_boxed() {
            // Zero-copy: hand the owned buffer straight back.
            // SAFETY: the tag says the `boxed` variant is active; the Box
            // is moved out by pointer read and `v` is forgotten so its
            // Drop never sees it.
            unsafe {
                let b: alloc::boxed::Box<str> = ptr::read(&*(*v.0.boxed).inner);
                core::mem::forget(v);
                alloc::string::String::from(b)
            }
        } else {
            v.as_str().to_owned()
        }
    }
}

impl AsRef<str> for TriStr {
    fn as_ref(&self) -> &str {
        self
    }
}

impl core::borrow::Borrow<str> for TriStr {
    #[inline(always)]
    fn borrow(&self) -> &str {
        self
    }
}

impl PartialEq<TriStr> for TriStr {
    #[inline]
    fn eq(&self, other: &TriStr) -> bool {
        // Fast path: both inline — the tags alone settle length inequality,
        // and equal tags pin the length, so the payloads compare directly
        // without constructing str slices through two derefs.
        let (ta, tb) = (self.tag(), other.tag());
        if ta & INLINE_FLAG != 0 && tb & INLINE_FLAG != 0 {
            if ta != tb {
                return false;
            }
            let len = (ta & INLINE_LEN) as usize;
            // SAFETY: both inline payloads hold exactly `len` initialized
            // bytes copied from valid UTF-8 sources.
            unsafe {
                let a = ptr::addr_of!(self.0.inline.data.bytes) as *const u8;
                let b = ptr::addr_of!(other.0.inline.data.bytes) as *const u8;
                return core::slice::from_raw_parts(a, len) == core::slice::from_raw_parts(b, len);
            }
        }
        self.as_str() == other.as_str()
    }
}

impl PartialEq<str> for TriStr {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for TriStr {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<alloc::string::String> for TriStr {
    fn eq(&self, other: &alloc::string::String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<TriStr> for alloc::string::String {
    fn eq(&self, other: &TriStr) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for TriStr {}

impl Hash for TriStr {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state)
    }
}

/// Byte-lexicographic order of `a[..min]` vs `b[..min]` for `1 <= min <= 16`
/// without a `memcmp` call: the first 8 bytes decide, then the overlapping
/// tail chunk. Chunks are byte-swapped (`rev` on AArch64) so an unsigned
/// machine compare visits byte 0 first — exactly `str` order. A PLT
/// `memcmp` call for a ≤16-byte compare costs more than the loads.
///
/// # Safety
/// `a` and `b` must each hold at least `min` readable bytes and `min` must
/// be in `1..=16`.
#[inline(always)]
unsafe fn prefix_cmp_inline(a: *const u8, b: *const u8, min: usize) -> core::cmp::Ordering {
    use core::cmp::Ordering;
    use core::ptr::read_unaligned as load;
    debug_assert!((1..=16).contains(&min));
    // SAFETY: every load addresses only bytes of [0, min): head and tail
    // pairs of matching width whose overlapping tail stays inside the
    // fragment, exactly like the register-built `From<&str>` loads.
    unsafe {
        if min >= 8 {
            let x = load(a as *const u64).swap_bytes();
            let y = load(b as *const u64).swap_bytes();
            if x != y {
                return if x < y {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            if min == 8 {
                return Ordering::Equal;
            }
            // The tail chunk overlaps chunk 0 in [min-8, 8); that overlap
            // just compared equal, so the first differing byte — if any —
            // lies in [8, min) and unsigned order after the byte swap
            // matches byte order there too.
            let x = load(a.add(min - 8) as *const u64).swap_bytes();
            let y = load(b.add(min - 8) as *const u64).swap_bytes();
            if x != y {
                return if x < y {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            Ordering::Equal
        } else if min >= 4 {
            let x = load(a as *const u32) as u64;
            let y = load(b as *const u32) as u64;
            let t = (min - 4) * 8;
            let x = (x | ((load(a.add(min - 4) as *const u32) as u64) << t)).swap_bytes();
            let y = (y | ((load(b.add(min - 4) as *const u32) as u64) << t)).swap_bytes();
            if x != y {
                return if x < y {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            Ordering::Equal
        } else if min >= 2 {
            let x = load(a as *const u16) as u64;
            let y = load(b as *const u16) as u64;
            let t = (min - 2) * 8;
            let x = (x | ((load(a.add(min - 2) as *const u16) as u64) << t)).swap_bytes();
            let y = (y | ((load(b.add(min - 2) as *const u16) as u64) << t)).swap_bytes();
            if x != y {
                return if x < y {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            Ordering::Equal
        } else {
            // min == 1
            (*a).cmp(&*b)
        }
    }
}

impl Ord for TriStr {
    #[inline]
    fn cmp(&self, other: &TriStr) -> core::cmp::Ordering {
        let (ta, tb) = (self.tag(), other.tag());
        if ta & INLINE_FLAG != 0 && tb & INLINE_FLAG != 0 {
            // Both inline: byte-lexicographic order over the common prefix,
            // then length — str::cmp's exact semantics, with the prefix
            // compared in registers instead of through two derefs and a
            // memcmp call.
            let (la, lb) = ((ta & INLINE_LEN) as usize, (tb & INLINE_LEN) as usize);
            let min = la.min(lb);
            // SAFETY: both payloads hold at least `min` initialized bytes.
            unsafe {
                let a = ptr::addr_of!(self.0.inline.data.bytes) as *const u8;
                let b = ptr::addr_of!(other.0.inline.data.bytes) as *const u8;
                if min <= 16 {
                    let ord = if min == 0 {
                        core::cmp::Ordering::Equal
                    } else {
                        prefix_cmp_inline(a, b, min)
                    };
                    if ord != core::cmp::Ordering::Equal {
                        return ord;
                    }
                } else {
                    let ord = core::slice::from_raw_parts(a, min)
                        .cmp(core::slice::from_raw_parts(b, min));
                    if ord != core::cmp::Ordering::Equal {
                        return ord;
                    }
                }
            }
            return la.cmp(&lb);
        }
        self.as_str().cmp(other.as_str())
    }
}

impl PartialOrd for TriStr {
    #[inline]
    fn partial_cmp(&self, other: &TriStr) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
    #[inline]
    fn lt(&self, other: &TriStr) -> bool {
        // sort's hot path: both-inline comparison returning bool directly,
        // without materializing the Ordering enum — and without a memcmp
        // call for the ≤16-byte prefixes that dominate short-key sorting.
        let (ta, tb) = (self.tag(), other.tag());
        if ta & INLINE_FLAG != 0 && tb & INLINE_FLAG != 0 {
            let (la, lb) = ((ta & INLINE_LEN) as usize, (tb & INLINE_LEN) as usize);
            let min = la.min(lb);
            // SAFETY: both payloads hold at least `min` initialized bytes.
            unsafe {
                if min == 0 {
                    // Empty common prefix: the tags already settled it.
                    return la < lb;
                }
                let a = ptr::addr_of!(self.0.inline.data.bytes) as *const u8;
                let b = ptr::addr_of!(other.0.inline.data.bytes) as *const u8;
                if min <= 16 {
                    match prefix_cmp_inline(a, b, min) {
                        core::cmp::Ordering::Less => return true,
                        core::cmp::Ordering::Greater => return false,
                        core::cmp::Ordering::Equal => {}
                    }
                } else {
                    match core::slice::from_raw_parts(a, min)
                        .cmp(core::slice::from_raw_parts(b, min))
                    {
                        core::cmp::Ordering::Less => return true,
                        core::cmp::Ordering::Greater => return false,
                        core::cmp::Ordering::Equal => {}
                    }
                }
            }
            la < lb
        } else {
            self.cmp(other) == core::cmp::Ordering::Less
        }
    }
    #[inline]
    fn gt(&self, other: &TriStr) -> bool {
        other.lt(self)
    }
}

impl fmt::Display for TriStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for TriStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for TriStr {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

#[cfg(feature = "serde")]
/// Construction for deserialized strings.
///
/// Length routing mirrors the two allocation sizes the heap variants
/// produce: exact-size `Box<str>` (owned, clone deep-copies) or
/// `Arc<str>` with its 16-byte counter header (shared, clone O(1)).
/// Both cost one allocation plus one copy; counters vs none is ~1ns.
///
/// The `len + 16 > 1024` crossover exists because the host allocator
/// (macOS libmalloc) serves requests up to 1KiB from its small-magazine
/// zone, which costs ~8ns more per alloc/copy/free cycle at 1KiB than
/// the region above it (measured: alloc+memcpy(1KiB)+free = 23.9ns for
/// a 1024B request vs 15.5ns for 1032-1152B). An exact-size `Box<str>`
/// cannot cross that boundary; `Arc`'s header can. Strings long enough
/// for the header to push the request past 1KiB therefore deserialize
/// shared (and their clones are O(1)); everything else stays owned.
///
/// `#[inline]`: without it the constructor stays an out-of-line sret
/// call in every visitor instantiation, adding a frame plus a 24-byte
/// result copy per deserialized field.
#[inline]
fn deser_from_str(v: &str) -> TriStr {
    if v.len() <= INLINE_CAP {
        TriStr::from(v)
    } else {
        deser_long(v)
    }
}

/// Out-of-line on purpose: keeping both heap constructions out of
/// `deser_from_str` lets the (much hotter) inline construction fit the
/// cross-crate inline budget at every visitor instantiation; the long
/// path is dominated by its allocation and copy anyway.
fn deser_long(v: &str) -> TriStr {
    if v.len() + 16 > 1024 {
        // 16 = the `Arc<str>` strong/weak counter header.
        TriStr::from_arc(Arc::from(v))
    } else {
        TriStr::from_boxed(alloc::boxed::Box::from(v))
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for TriStr {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TriStrVisitor;

        impl<'de> serde::de::Visitor<'de> for TriStrVisitor {
            type Value = TriStr;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<TriStr, E> {
                #[cfg(feature = "stats")]
                crate::stats::SRC_DESER.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                Ok(deser_from_str(v))
            }

            fn visit_string<E: serde::de::Error>(
                self,
                v: alloc::string::String,
            ) -> Result<TriStr, E> {
                #[cfg(feature = "stats")]
                crate::stats::SRC_DESER.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                Ok(TriStr::from(v))
            }

            fn visit_borrowed_str<E: serde::de::Error>(self, v: &'de str) -> Result<TriStr, E> {
                #[cfg(feature = "stats")]
                crate::stats::SRC_DESER.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                Ok(deser_from_str(v))
            }
        }

        deserializer.deserialize_str(TriStrVisitor)
    }
}

#[cfg(feature = "specta")]
impl specta::Type for TriStr {
    fn definition(_types: &mut specta::Types) -> specta::datatype::DataType {
        use specta::datatype::{DataType, Primitive};
        DataType::Primitive(Primitive::str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn _assert_send_sync<T: Send + Sync + 'static>() {}

    #[test]
    fn auto_traits() {
        _assert_send_sync::<TriStr>();
    }

    #[test]
    fn size_matches_std_string() {
        assert_eq!(
            core::mem::size_of::<TriStr>(),
            core::mem::size_of::<String>()
        );
    }

    #[test]
    fn tag_offset_is_23() {
        assert_eq!(TAG_OFFSET, 23);
    }

    #[test]
    fn heap_variant_reports_arc_count() {
        // exact-capacity adoption -> owned; clones -> shared
        let v = TriStr::from("x".repeat(64));
        assert!(v.is_boxed());
        assert_eq!(v.shared_count(), None);
        let c = v.clone();
        assert_eq!(c.shared_count(), Some(1));
        drop(c);
        // from_&str stays shared
        let a = TriStr::from("x".repeat(64).as_str());
        assert_eq!(a.shared_count(), Some(1));
    }
}
