use std::sync::OnceLock;
use std::sync::atomic::AtomicU64;

use crate::stores::{BuildError, SetMaxSizeError};

/// Cache-line size used for padding. Covers both x86_64 (64 B + Intel adjacent-line prefetch)
/// and Apple Silicon (128 B L1 line). Matches the `repr(align)` on `CachePadded`.
/// Note: `#[repr(align(…))]` only accepts integer literals, so this constant cannot be used
/// directly in the attribute — the literal `128` in `CachePadded` must match it by hand.
pub(crate) const CACHE_LINE: usize = 128;
const _: () = assert!(
    CACHE_LINE == 128,
    "CachePadded repr(align) literal must match CACHE_LINE"
);

/// Aligns its payload to a cache line so adjacent elements in a slice
/// can't false-share. Same pattern as `crossbeam_utils::CachePadded`;
/// rolled here to avoid a new dependency.
#[repr(align(128))]
pub(crate) struct CachePadded<T>(pub T);

impl<T> std::ops::Deref for CachePadded<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}
impl<T> std::ops::DerefMut for CachePadded<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}

/// Per-shard state. Plain struct — alignment is the caller's responsibility
/// (in practice always `CachePadded<Shard<S>>`). The lock word and the
/// hit/miss counters intentionally share a cache line: they are touched by
/// the same op (a `cache_get` acquires the lock and then bumps a counter),
/// so spatial locality is a win. Counters use `Relaxed` atomics; on stores
/// that allow concurrent readers (read-lock paths), increments can race —
/// this is intentional, trading exactness for lower overhead.
pub(crate) struct Shard<S> {
    pub lock: parking_lot::RwLock<S>,
    pub hits: AtomicU64,
    pub misses: AtomicU64,
    /// Per-shard eviction count. Co-located with `hits`/`misses` for the same reason:
    /// a thread bumping this has just taken `lock`, so it already owns this cache line
    /// exclusively.
    ///
    /// Not every consuming store uses this field. The ttl/expiring family (sharded ttl,
    /// expiring, lru_ttl, expiring_lru) counts shard-level evictions here and sums the
    /// field across shards for its metrics. Sharded lru instead counts evictions via the
    /// inner `LruCache`'s own per-store counter (read back through `cache_evictions()`),
    /// and sharded unbound has no eviction concept at all; for those two the field is
    /// intentionally left unused. Keeping one shared field (rather than a type-level split
    /// of `Shard`) is a deliberate simplification.
    pub evictions: AtomicU64,
}

impl<S> Shard<S> {
    pub fn new(store: S) -> Self {
        Self {
            lock: parking_lot::RwLock::new(store),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
        }
    }
}

/// The default shard count: `available_parallelism() * 4`, clamped to `[8, 1024]` and rounded
/// up to a power of two.
///
/// The host CPU topology is sampled exactly once per process (memoized in a `OnceLock`) on the
/// first call, then reused for every cache built afterward. A later change to the effective CPU
/// count -- for example a cgroup/container CPU-quota adjustment made after the first sample --
/// does not affect the shard count of subsequently built caches; they all see the value latched
/// at first call.
pub(crate) fn default_shard_count() -> usize {
    static DEFAULT_SHARD_COUNT: OnceLock<usize> = OnceLock::new();
    *DEFAULT_SHARD_COUNT.get_or_init(|| {
        let count = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .saturating_mul(4);
        // `clamp(8, 1024)` bounds the input to [8, 1024]; 1024 is itself a power of two, so
        // `next_power_of_two()` returns at most 1024 and can never overflow. (The user-supplied
        // path in `checked_shard_count` has no upper bound, so it uses `checked_next_power_of_two`.)
        count.clamp(8, 1024).next_power_of_two()
    })
}

/// Default shard count for the *default* (unconfigured) shard-count path, scaled down for
/// small `max_size` values.
///
/// Without this, the default shard count is `available_parallelism() * 4` clamped to
/// `[8, 1024]`, with no reference to `max_size` at all. Combined with the 16-entries-per-shard
/// floor in [`per_shard_cap_from_total`] and eager per-shard allocation, `ShardedLruCache::new(100)`
/// on a 64-core box would build 256 shards x 16 = 4096 effective capacity, preallocating 256
/// hash tables plus 256 `Vec`s for a cache asked to hold only 100 entries.
///
/// This helper caps the shard count itself to roughly `max_size / 16` (rounded up to a power
/// of two), never exceeding [`default_shard_count`] and never going below 1, so that a bounded
/// cache ends up with each shard holding roughly 16 entries rather than 16 entries times an
/// oversized shard count.
///
/// An explicit `.shards(n)` on a builder remains authoritative; this helper is only consulted
/// on the *default* (no explicit shard count given) path.
pub(crate) fn default_shard_count_for_capacity(max_size: Option<usize>) -> usize {
    match max_size {
        Some(n) => (n / 16).next_power_of_two().clamp(1, default_shard_count()),
        None => default_shard_count(),
    }
}

/// Compute the per-shard capacity for a given total and shard count, applying the
/// same policy as the sharded LRU builders: ceiling division (`div_ceil`) with a
/// minimum of 16 per shard when `n_shards > 1`.
///
/// Returns `(per_shard_cap, total_cap)`, where `total_cap = n_shards * per_shard_cap`.
/// The `total_cap` may exceed `total` when the 16-per-shard floor is in effect.
///
/// Returns [`SetMaxSizeError::CapacityOverflow`] when `n_shards * per_shard_cap` does not fit
/// in a `usize`. That happens for a `total` close to `usize::MAX` on a multi-shard cache:
/// `div_ceil` rounds the per-shard share up, and multiplying the rounded share back by
/// `n_shards` then exceeds `usize::MAX`. It is the fallible entry point behind
/// `try_set_max_size`, which must not panic.
pub(crate) fn checked_per_shard_cap_from_total(
    total: usize,
    n_shards: usize,
) -> Result<(usize, usize), SetMaxSizeError> {
    let mut per_shard = total.div_ceil(n_shards);
    if n_shards > 1 {
        per_shard = per_shard.max(16);
    }
    let total_cap = n_shards
        .checked_mul(per_shard)
        .ok_or(SetMaxSizeError::CapacityOverflow)?;
    Ok((per_shard, total_cap))
}

/// Compute the per-shard capacity for a given total and shard count, applying the
/// same policy as the sharded LRU builders: ceiling division (`div_ceil`) with a
/// minimum of 16 per shard when `n_shards > 1`.
///
/// Returns `(per_shard_cap, total_cap)`, where `total_cap = n_shards * per_shard_cap`.
/// The `total_cap` may exceed `total` when the 16-per-shard floor is in effect.
///
/// # Panics
///
/// Panics if `n_shards * per_shard_cap` overflows `usize`. Callers that must not panic go
/// through [`checked_per_shard_cap_from_total`] instead.
pub(crate) fn per_shard_cap_from_total(total: usize, n_shards: usize) -> (usize, usize) {
    checked_per_shard_cap_from_total(total, n_shards)
        .expect("per_shard_cap_from_total: n_shards * per_shard overflows usize")
}

pub(crate) fn checked_shard_count(shards: Option<usize>) -> Result<usize, BuildError> {
    if let Some(0) = shards {
        return Err(BuildError::InvalidValue {
            field: "shards",
            reason: "shard count must be >= 1",
        });
    }
    shards
        .unwrap_or_else(default_shard_count)
        .checked_next_power_of_two()
        .ok_or(BuildError::InvalidValue {
            field: "shards",
            reason: "rounded shard count overflows usize",
        })
}

#[inline]
pub(crate) fn shard_index(hash: u64, mask: usize) -> usize {
    (hash >> 32) as usize & mask
}

/// Encode a TTL into a nanosecond atomic. A zero duration encodes as `0`
/// (expiry disabled / no expiry).
#[cfg(feature = "time_stores")]
#[inline]
pub(crate) fn encode_ttl(ttl: crate::time::Duration) -> u64 {
    ttl.as_nanos().min(u64::MAX as u128) as u64
}

/// Decode the nanosecond atomic into an optional TTL. `0` means expiry is
/// disabled (entries never expire), so it decodes to `None`.
#[cfg(feature = "time_stores")]
#[inline]
pub(crate) fn decode_ttl(nanos: u64) -> Option<crate::time::Duration> {
    if nanos == 0 {
        None
    } else {
        Some(crate::time::Duration::from_nanos(nanos))
    }
}

/// Trait for types that deterministically map a key to a `u64` shard hash.
///
/// No `K: Hash` bound on the trait itself — custom impls can partition by
/// arbitrary logic (e.g. numeric range, string prefix, etc.).
///
/// # Any `BuildHasher` is a `ShardHasher`
///
/// A blanket impl covers every [`std::hash::BuildHasher`] that is `Clone + Send + Sync +
/// 'static`, for every `K: Hash`, hashing the key explicitly: build a
/// [`Hasher`](std::hash::Hasher) with
/// [`build_hasher`](std::hash::BuildHasher::build_hasher), feed the key to it with
/// [`Hash::hash`](std::hash::Hash::hash), then [`finish`](std::hash::Hasher::finish) it. The same
/// hasher value therefore works on both cache families: what
/// [`LruCacheBuilder::hasher`](crate::LruCacheBuilder::hasher) accepts,
/// [`ShardedLruCacheBuilder::hasher`](crate::ShardedLruCacheBuilder::hasher) and its five
/// siblings accept too.
///
/// ```rust
/// use cached::ShardedLruCache;
/// use std::hash::RandomState;
///
/// let cache = ShardedLruCache::<u64, u64>::builder()
///     .max_size(1024)
///     .hasher(RandomState::new())
///     .build()
///     .unwrap();
/// ```
///
/// [`DefaultShardHasher`] itself implements `BuildHasher` and reaches `ShardHasher` through
/// this blanket impl, so it is equally usable as the hash builder of a non-sharded store.
///
/// Coherence makes the blanket impl exclusive: a type that implements `BuildHasher` cannot also
/// carry a hand-written `ShardHasher` impl (that is a duplicate-impl error). Custom shard
/// routing (numeric range, string prefix, tenant id) therefore belongs on a type that does
/// **not** implement `BuildHasher`.
///
/// # Cost of a hand-written impl: the inherent lookups disappear
///
/// A hand-written `ShardHasher` is the supported way to control shard routing, but it is not
/// free. The six inherent lookups on every sharded store (`get`, `remove`, `remove_entry`,
/// `delete`, `contains`, `peek`) are bounded on [`BorrowedKeyRouting`], which is exactly
/// `BuildHasher`. On a store whose `H` does not implement `BuildHasher` those inherent methods
/// do not exist at all -- not just the borrowed-key form `get(&"k"[..])`, but the plain owned-key
/// call `cache.get(&key)` too, since the bound is unconditional rather than predicated on
/// `Q != K`. There is no method-resolution fallback: the inherent method is picked by name and
/// then fails its bound, so importing a trait does not rescue the same call site.
///
/// The escape hatch is to call the trait forms, which take an owned-key reference and return
/// `Result<_, Infallible>`:
///
/// ```rust
/// use cached::{ConcurrentCachePeek, ConcurrentCachedExt, ShardHasher, ShardedUnboundCache};
///
/// #[derive(Clone)]
/// struct TenantHasher;
/// impl ShardHasher<u64> for TenantHasher {
///     fn shard_hash(&self, key: &u64) -> u64 {
///         key.wrapping_mul(0x9e3779b97f4a7c15)
///     }
/// }
///
/// let cache = ShardedUnboundCache::<u64, u64>::builder()
///     .hasher(TenantHasher)
///     .build()
///     .unwrap();
/// cache.set(7, 70);
///
/// // `cache.get(&7)` does not compile here: the inherent method needs `H: BuildHasher`.
/// assert_eq!(ConcurrentCachedExt::get(&cache, &7).unwrap(), Some(70));
/// assert_eq!(ConcurrentCachePeek::peek(&cache, &7).unwrap(), Some(70));
/// ```
///
/// If shard routing is not the point and only the hash function is, pass a `BuildHasher`
/// (for example `std::hash::RandomState` or `ahash::RandomState`) to the builder's `hasher`
/// instead and keep all six inherent lookups.
///
/// # Shard selection
///
/// The shard index is derived from the upper 32 bits of the returned hash:
/// `(hash >> 32) & shard_mask`. [`DefaultShardHasher`] (ahash when the `ahash`
/// feature is enabled, otherwise std `RandomState`) produces high-quality bits
/// in both halves. Custom implementations should ensure the
/// **upper** 32 bits are well-distributed across keys, not just the lower bits.
///
/// The two hashers reached through the blanket impl that this crate builds on both satisfy
/// that contract: `std::hash::RandomState` finishes a SipHash-1-3 state, and
/// `ahash::RandomState` finishes an aHash state; both diffuse key entropy across
/// all 64 bits, so the upper half is as well-distributed as the lower half. Reaching
/// `ShardHasher` through `BuildHasher` is **not** by itself a guarantee, though: the warning
/// below applies unchanged to a hand-written `BuildHasher` whose `finish` leaves the high bits
/// constant (a `Hasher` accumulating into a `u32` widened to `u64`, for instance).
///
/// # Warning: zero upper bits route everything to shard 0
///
/// If `shard_hash` returns a value whose upper 32 bits are always zero, every key
/// will land on shard 0, defeating the purpose of sharding entirely. A common
/// mistake is returning a bare integer identity:
///
/// ```rust
/// use cached::ShardHasher;
///
/// // BAD -- `key as u64` for small integer keys leaves bits 32-63 all zero.
/// // All entries land on shard 0 regardless of the configured shard count.
/// #[derive(Clone)]
/// struct IdentityHasher;
/// impl ShardHasher<u32> for IdentityHasher {
///     fn shard_hash(&self, key: &u32) -> u64 {
///         *key as u64  // upper 32 bits are always 0!
///     }
/// }
/// ```
///
/// Always mix or multiply the value so entropy is spread into the upper 32 bits.
///
/// # Example
///
/// ```rust
/// use cached::ShardHasher;
///
/// /// Distributes `u64` keys using Fibonacci hashing (`key * 2^64/φ`).
/// /// Ensures the upper 32 bits (used for shard selection) are well-distributed.
/// #[derive(Clone)]
/// struct FibHasher;
/// impl ShardHasher<u64> for FibHasher {
///     fn shard_hash(&self, key: &u64) -> u64 {
///         key.wrapping_mul(0x9e3779b97f4a7c15)
///     }
/// }
/// ```
///
/// The `'static` bound is required because the hasher is stored inside `Arc<Inner>`,
/// and the `Arc` is cloned across threads — a borrowed or lifetime-parameterized hasher
/// would prevent the cache from being `'static` and therefore from being shared via
/// `thread::spawn` or stored in a `static`.
pub trait ShardHasher<K>: Clone + Send + Sync + 'static {
    fn shard_hash(&self, key: &K) -> u64;
}

/// Every thread-safe [`BuildHasher`](std::hash::BuildHasher) routes keys by feeding the key to a
/// freshly built [`Hasher`](std::hash::Hasher) and finishing it.
///
/// This is what lets a single hasher value be handed to both the single-owner builders
/// (`S: BuildHasher`) and the sharded builders (`H: ShardHasher<K>`). It also means the two
/// trait sets are no longer independent: implementing `BuildHasher` on a type commits it to
/// this shard-routing behavior, since coherence rejects a second, hand-written `ShardHasher`
/// impl for the same type.
///
/// The construction is written out rather than delegated to
/// [`BuildHasher::hash_one`](std::hash::BuildHasher::hash_one) on purpose.
/// `hash_one` is an overridable provided method whose implementation is allowed to dispatch on
/// its static type argument `T`, so `hash_one::<&K>` and `hash_one::<&Q>` are not required to
/// agree even when `K: Borrow<Q>` and the two values hash identically. `ahash::RandomState` does
/// exactly that: it routes through a `CallHasher` table with specialized impls for some reference
/// types (`&u8`..`&i64`, `&u128`/`&i128`/`&usize`/`&isize`) and not others (`&str`, `&String`,
/// `&[u8]`, `&Vec<u8>`), enabled whenever its `specialize` cfg is on. A newtype key such as
/// `struct UserId(u64)` with `Borrow<u64>` would then route its owned inserts and its borrowed
/// lookups to different shards. Building the `Hasher` explicitly depends only on the `Hash` impl,
/// which the `Borrow` contract already requires to agree, so owned and borrowed routing match for
/// every `BuildHasher`. The `shard_of_borrowed` helper on each sharded store uses the identical
/// construction.
impl<K, S> ShardHasher<K> for S
where
    K: std::hash::Hash,
    S: std::hash::BuildHasher + Clone + Send + Sync + 'static,
{
    // Deliberately not `hash_one`, for the reason spelled out in the impl docs above: it may
    // dispatch on its static type argument, which is what would let an owned key and an
    // equivalent borrowed key route to different shards.
    #[allow(clippy::manual_hash_one)]
    fn shard_hash(&self, key: &K) -> u64 {
        use std::hash::Hasher as _;
        let mut hasher = self.build_hasher();
        key.hash(&mut hasher);
        hasher.finish()
    }
}

/// Exactly equivalent to [`BuildHasher`](std::hash::BuildHasher): a blanket impl covers every
/// `BuildHasher` and nothing else can implement it.
///
/// It is the bound on the six inherent lookups (`get`, `remove`, `remove_entry`, `delete`,
/// `contains`, `peek`) of the six sharded stores. Those methods take a `&Q where K: Borrow<Q>`
/// and hash the `&Q` with the store's own hash builder, which agrees with the owned key's
/// routing only when the store's `ShardHasher` impl is the blanket `BuildHasher` one.
///
/// The bound is unconditional, not predicated on `Q != K`, so on a store built with a
/// hand-written [`ShardHasher`] all six inherent methods disappear entirely: the owned-key call
/// `cache.get(&key)` fails exactly like the borrowed-key call `cache.get(&key[..])`. There is no
/// method-resolution fallback either, because the inherent method is selected by name before its
/// bound is checked, so importing a trait does not rescue the same call site. Such a store is
/// used through the owned-key trait methods instead, which return `Result<_, Infallible>`:
/// [`ConcurrentCachedExt`](crate::ConcurrentCachedExt) supplies
/// `get`/`remove`/`remove_entry`/`delete`/`contains` and
/// [`ConcurrentCachePeek`](crate::ConcurrentCachePeek) supplies `peek`, e.g.
/// `ConcurrentCachedExt::get(&cache, &key).unwrap()` and
/// `ConcurrentCachePeek::peek(&cache, &key).unwrap()`.
///
/// The trait exists only so that failure explains itself. Bounding the methods on `BuildHasher`
/// directly produces a bare `E0277` naming `BuildHasher`, which says nothing about shard routing
/// or about the owned-key calls that do work.
#[diagnostic::on_unimplemented(
    message = "the inherent `get`/`remove`/`remove_entry`/`delete`/`contains`/`peek` on a sharded store need a `BuildHasher`-based shard hasher",
    label = "`{Self}` implements `ShardHasher` directly, so this store has no inherent key lookups",
    note = "this applies to the owned-key call `cache.get(&key)` too, not just borrowed keys such as `cache.get(key.as_str())`",
    note = "use the trait methods instead: `ConcurrentCachedExt::get(&cache, &key).unwrap()` and the matching `remove`/`remove_entry`/`delete`/`contains`, plus `ConcurrentCachePeek::peek(&cache, &key).unwrap()` for `peek`",
    note = "they return `Result<_, Infallible>`, so `.unwrap()` recovers the inherent method's plain return type"
)]
pub trait BorrowedKeyRouting: std::hash::BuildHasher {}

impl<T: std::hash::BuildHasher> BorrowedKeyRouting for T {}

/// Default shard hasher backed by `ahash::RandomState` (or `std::collections::hash_map::RandomState`
/// when the `ahash` feature is disabled).
///
/// It implements [`BuildHasher`](std::hash::BuildHasher) and picks up
/// [`ShardHasher<K>`] for every `K: Hash` through the blanket impl above, so it is also a valid
/// hash builder for the non-sharded stores and for a plain
/// [`HashMap`](std::collections::HashMap).
#[derive(Clone)]
pub struct DefaultShardHasher(
    #[cfg(feature = "ahash")] ahash::RandomState,
    #[cfg(not(feature = "ahash"))] std::collections::hash_map::RandomState,
);

impl Default for DefaultShardHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultShardHasher {
    #[must_use]
    pub fn new() -> Self {
        #[cfg(feature = "ahash")]
        {
            Self(ahash::RandomState::new())
        }
        #[cfg(not(feature = "ahash"))]
        {
            Self(std::collections::hash_map::RandomState::new())
        }
    }
}

impl std::hash::BuildHasher for DefaultShardHasher {
    type Hasher = <crate::stores::DefaultHashBuilder as std::hash::BuildHasher>::Hasher;

    fn build_hasher(&self) -> Self::Hasher {
        std::hash::BuildHasher::build_hasher(&self.0)
    }
}

mod expiring;
mod expiring_lru;
mod lru;
mod unbound;

#[cfg(feature = "time_stores")]
mod lru_ttl;
#[cfg(feature = "time_stores")]
mod ttl;

pub use expiring::{ShardedExpiringCache, ShardedExpiringCacheBuilder};
pub use expiring_lru::{ShardedExpiringLruCache, ShardedExpiringLruCacheBuilder};
pub use lru::{ShardedLruCache, ShardedLruCacheBuilder};
pub use unbound::{ShardedUnboundCache, ShardedUnboundCacheBuilder};

#[cfg(feature = "time_stores")]
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
pub use ttl::{ShardedTtlCache, ShardedTtlCacheBuilder};

#[cfg(feature = "time_stores")]
#[cfg_attr(docsrs, doc(cfg(feature = "time_stores")))]
pub use lru_ttl::{ShardedLruTtlCache, ShardedLruTtlCacheBuilder};

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{align_of, size_of};

    #[test]
    fn cache_padded_is_aligned() {
        assert_eq!(align_of::<CachePadded<u8>>(), CACHE_LINE);
        assert_eq!(size_of::<CachePadded<u8>>() % CACHE_LINE, 0);
    }

    #[test]
    fn default_shard_hasher_works() {
        let h = DefaultShardHasher::new();
        let v1 = h.shard_hash(&42u64);
        let v2 = h.shard_hash(&42u64);
        assert_eq!(v1, v2);
        // different keys should (almost certainly) produce different hashes
        let v3 = h.shard_hash(&43u64);
        assert_ne!(v1, v3);
    }

    /// `DefaultShardHasher` reaches `ShardHasher` through the `BuildHasher` blanket impl and
    /// nothing else. `shard_hash` builds a `Hasher`, feeds the key to it and finishes it, which
    /// is exactly the provided `BuildHasher::hash_one` body; `DefaultShardHasher` does not
    /// override `hash_one`, so the two agree here. A second, explicit
    /// `ShardHasher for DefaultShardHasher` impl alongside the blanket one would be a
    /// duplicate-impl error, so this compiling is itself the single-path check.
    #[test]
    fn default_shard_hasher_routes_through_build_hasher() {
        use std::hash::BuildHasher;
        let h = DefaultShardHasher::new();
        assert_eq!(h.shard_hash(&42u64), h.hash_one(42u64));
        assert_eq!(h.shard_hash(&"key"), h.hash_one("key"));
    }

    /// A `Clone`-implementing custom `ShardHasher` satisfies the `ShardHasher: Clone`
    /// supertrait bound (item 11). If this compiles, the bound is enforced correctly.
    #[test]
    fn custom_shard_hasher_requires_clone() {
        #[derive(Clone)]
        struct ConstHasher;
        impl ShardHasher<u64> for ConstHasher {
            fn shard_hash(&self, key: &u64) -> u64 {
                // Fibonacci hashing so upper bits are populated.
                key.wrapping_mul(0x9e3779b97f4a7c15)
            }
        }
        let h = ConstHasher;
        let h2 = h.clone();
        assert_eq!(h.shard_hash(&1), h2.shard_hash(&1));
    }

    /// `ShardHasher` has `Clone` as a supertrait - verify a non-Clone type cannot
    /// satisfy the bound. This is a compile-time-only check: a `Clone` bound on the
    /// trait means the trait object is only constructable for `Clone` types.
    #[allow(dead_code)]
    fn assert_shard_hasher_requires_clone<H: ShardHasher<u64>>(_h: H) {}
    #[allow(dead_code)]
    fn check_shard_hasher_supertrait() {
        // DefaultShardHasher derives Clone, so it satisfies the bound.
        assert_shard_hasher_requires_clone(DefaultShardHasher::new());
    }

    /// `checked_per_shard_cap_from_total` reports the total-capacity overflow as an error
    /// instead of panicking, so a `try_set_max_size` built on it stays total.
    #[test]
    fn checked_per_shard_cap_reports_overflow_instead_of_panicking() {
        // usize::MAX.div_ceil(16) rounds the per-shard share up, so multiplying it back by
        // 16 lands one past usize::MAX.
        assert_eq!(
            checked_per_shard_cap_from_total(usize::MAX, 16),
            Err(SetMaxSizeError::CapacityOverflow)
        );
        assert_eq!(
            checked_per_shard_cap_from_total(usize::MAX - 1, 1024),
            Err(SetMaxSizeError::CapacityOverflow)
        );
    }

    /// A single shard divides by one, so no rounding and no product: every total is
    /// representable and the checked helper agrees with the requested size exactly.
    #[test]
    fn checked_per_shard_cap_accepts_max_total_on_one_shard() {
        assert_eq!(
            checked_per_shard_cap_from_total(usize::MAX, 1),
            Ok((usize::MAX, usize::MAX))
        );
    }

    /// The checked helper applies the same ceiling-division and 16-per-shard-floor policy as
    /// the panicking wrapper for every non-overflowing input.
    #[test]
    fn checked_per_shard_cap_matches_panicking_wrapper() {
        for (total, n_shards) in [(1usize, 1usize), (100, 8), (4, 16), (1024, 16), (17, 4)] {
            assert_eq!(
                checked_per_shard_cap_from_total(total, n_shards),
                Ok(per_shard_cap_from_total(total, n_shards)),
                "policy diverged for total={total} n_shards={n_shards}"
            );
        }
    }

    #[test]
    fn shard_has_evictions_counter_initialized_to_zero() {
        let shard = Shard::new(0u32);
        assert_eq!(
            shard.evictions.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        shard
            .evictions
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            shard.evictions.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn default_shard_count_is_stable_across_calls() {
        // `default_shard_count` caches its result in a `OnceLock`; repeated calls
        // must return the exact same value (process-stable), and it must always
        // land in the documented [8, 1024] power-of-two range.
        let first = default_shard_count();
        let second = default_shard_count();
        assert_eq!(first, second);
        assert!((8..=1024).contains(&first));
        assert!(first.is_power_of_two());
    }

    #[test]
    fn default_shard_count_for_capacity_none_matches_default() {
        assert_eq!(
            default_shard_count_for_capacity(None),
            default_shard_count()
        );
    }

    #[test]
    fn default_shard_count_for_capacity_small_sizes_floor_to_one() {
        // n/16 == 0 for n in [0, 15], and next_power_of_two(0) == 1 (not 0), so the
        // clamp lower bound of 1 is what actually saves us here -- confirm it does.
        assert_eq!(default_shard_count_for_capacity(Some(1)), 1);
        assert_eq!(default_shard_count_for_capacity(Some(15)), 1);
        // 16/16 == 1, next_power_of_two(1) == 1.
        assert_eq!(default_shard_count_for_capacity(Some(16)), 1);
        // 17/16 == 1 (integer division truncates), next_power_of_two(1) == 1.
        assert_eq!(default_shard_count_for_capacity(Some(17)), 1);
    }

    #[test]
    fn default_shard_count_for_capacity_scales_with_size() {
        // 100/16 == 6, next_power_of_two(6) == 8, and 8 <= default_shard_count()
        // (whose minimum is 8), so the clamp never kicks in here.
        assert_eq!(default_shard_count_for_capacity(Some(100)), 8);
    }

    #[test]
    fn default_shard_count_for_capacity_clamps_at_max() {
        // usize::MAX / 16 is huge; next_power_of_two() of that is still hugely larger
        // than default_shard_count()'s max of 1024, so the upper clamp bound applies
        // and the result equals default_shard_count() exactly.
        assert_eq!(
            default_shard_count_for_capacity(Some(usize::MAX)),
            default_shard_count()
        );
    }

    #[test]
    fn default_shard_count_for_capacity_never_overflows_or_returns_zero() {
        for n in [0usize, 1, 15, 16, 17, 100, usize::MAX] {
            let result = default_shard_count_for_capacity(Some(n));
            assert!(result >= 1, "result for {n} was 0");
            assert!(result <= default_shard_count());
        }
    }
}
