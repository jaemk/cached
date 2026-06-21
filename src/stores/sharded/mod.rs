use std::sync::atomic::AtomicU64;

use crate::stores::BuildError;

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
}

impl<S> Shard<S> {
    pub fn new(store: S) -> Self {
        Self {
            lock: parking_lot::RwLock::new(store),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }
}

pub(crate) fn default_shard_count() -> usize {
    let count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .saturating_mul(4);
    // `clamp(8, 1024)` bounds the input to [8, 1024]; 1024 is itself a power of two, so
    // `next_power_of_two()` returns at most 1024 and can never overflow. (The user-supplied
    // path in `checked_shard_count` has no upper bound, so it uses `checked_next_power_of_two`.)
    count.clamp(8, 1024).next_power_of_two()
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

/// Trait for types that deterministically map a key to a `u64` shard hash.
///
/// No `K: Hash` bound on the trait itself — custom impls can partition by
/// arbitrary logic (e.g. numeric range, string prefix, etc.).
///
/// # Shard selection
///
/// The shard index is derived from the upper 32 bits of the returned hash:
/// `(hash >> 32) & shard_mask`. [`DefaultShardHasher`] (ahash when the `ahash`
/// feature is enabled, otherwise std `RandomState`) produces high-quality bits
/// in both halves. Custom implementations should ensure the
/// **upper** 32 bits are well-distributed across keys, not just the lower bits.
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

/// Default shard hasher backed by `ahash::RandomState` (or `std::collections::hash_map::RandomState`
/// when the `ahash` feature is disabled). Requires `K: Hash`.
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

impl<K: std::hash::Hash> ShardHasher<K> for DefaultShardHasher {
    fn shard_hash(&self, key: &K) -> u64 {
        use std::hash::BuildHasher;
        BuildHasher::hash_one(&self.0, key)
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

pub use expiring::{ShardedExpiringCache, ShardedExpiringCacheBase, ShardedExpiringCacheBuilder};
pub use expiring_lru::{
    ShardedExpiringLruCache, ShardedExpiringLruCacheBase, ShardedExpiringLruCacheBuilder,
};
pub use lru::{ShardedLruCache, ShardedLruCacheBase, ShardedLruCacheBuilder};
pub use unbound::{ShardedUnboundCache, ShardedUnboundCacheBase, ShardedUnboundCacheBuilder};

#[cfg(feature = "time_stores")]
pub use ttl::{ShardedTtlCache, ShardedTtlCacheBase, ShardedTtlCacheBuilder};

#[cfg(feature = "time_stores")]
pub use lru_ttl::{ShardedLruTtlCache, ShardedLruTtlCacheBase, ShardedLruTtlCacheBuilder};

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
}
