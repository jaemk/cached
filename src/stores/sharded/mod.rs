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

/// The one hash construction every [`BuildHasher`](std::hash::BuildHasher)-backed shard-routing
/// path uses, for both owned and borrowed keys.
///
/// It is not every routing path: a hand-written [`ShardHasher`] routes through its own
/// `shard_hash` bodies and never reaches this function. Such a router keeps owned and borrowed
/// routing in agreement by satisfying the cross-impl contract documented on [`ShardHasher`],
/// which the compiler cannot check. What follows applies to hashers that reach `ShardHasher`
/// through the blanket `BuildHasher` impl, which is every default-path store.
///
/// Owned routing (the blanket [`ShardHasher`] impl) and borrowed routing (`shard_of_borrowed` on
/// each sharded store) must produce the same value for keys that compare equal, or an entry
/// becomes unreachable through the form it was not inserted with. Routing them through a single
/// function is what makes that hold: the two paths cannot drift, because there is only one path.
///
/// The guarantee is not limited to *selecting* the shard. Once inside the shard, the store still
/// has to probe its own map, and that probe needs to agree with routing for the same reason
/// -- a `HashMap::get` that hashes a borrowed key differently from how the owned key was routed
/// and inserted would defeat the point of matching shards. Two separate mechanisms hold that
/// line, one per shard payload, and both are load-bearing:
///
/// * `ShardedUnboundCache`, `ShardedTtlCache` and `ShardedExpiringCache` hold a `HashMap` whose
///   own hash builder is [`DefaultShardHasher`], so the intra-shard probe goes through this
///   exact construction too, not through a second, independently-specializing
///   `BuildHasher::hash_one` call.
/// * `ShardedLruCache`, `ShardedLruTtlCache` and `ShardedExpiringLruCache` hold a
///   [`LruCache`](crate::LruCache), whose hash builder is
///   [`DefaultHashBuilder`](crate::DefaultHashBuilder), a different type. They are safe because
///   `LruCache::hash` (`src/stores/lru.rs`) hand-builds its `Hasher` exactly the way this
///   function does instead of calling `hash_one`. Rewriting that body into `hash_one` (the same
///   cleanup clippy's `manual_hash_one` lint asks for here, hence the `allow` below)
///   reintroduces the misroute on nightly for these three stores; the note on `LruCache::hash`
///   says so at the site.
///
/// Deliberately not [`BuildHasher::hash_one`](std::hash::BuildHasher::hash_one). That is an
/// overridable provided method whose implementation may dispatch on its static type argument, so
/// `hash_one::<&K>` and `hash_one::<&Q>` are not required to agree even when the two values hash
/// identically. `ahash::RandomState` does dispatch that way, through a `CallHasher` table with
/// specialized impls for some reference types (`&u8`..`&i64`, `&u128`/`&i128`/`&usize`/`&isize`)
/// and not others (`&str`, `&String`, `&[u8]`, `&Vec<u8>`), active whenever its `specialize` cfg
/// is on (any nightly compiler). A newtype key such as `struct UserId(u64)` with `Borrow<u64>`
/// would then route its owned inserts and its borrowed lookups to different shards. Building the
/// `Hasher` explicitly depends only on the `Hash` impl, which the `Borrow` contract already
/// requires to agree.
// `manual_hash_one` fires on exactly the construction this function exists to guarantee.
#[allow(clippy::manual_hash_one)]
#[inline]
pub(crate) fn routing_hash<H, Q>(hasher: &H, k: &Q) -> u64
where
    H: std::hash::BuildHasher,
    Q: std::hash::Hash + ?Sized,
{
    use std::hash::Hasher as _;
    let mut hasher = hasher.build_hasher();
    k.hash(&mut hasher);
    hasher.finish()
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
/// No `K: Hash` bound on the trait itself, so custom impls can partition by arbitrary logic
/// (numeric range, string prefix, tenant id). That relaxation is about the routing logic only:
/// every sharded store's own `impl` block still requires `K: Hash + Eq` (plus `K: Clone` on the
/// three LRU-bounded stores), so a non-`Hash` key type remains unusable no matter what the
/// router does.
///
/// # Any `BuildHasher` is a `ShardHasher`
///
/// A blanket impl covers every [`std::hash::BuildHasher`] that is `Clone + Send + Sync +
/// 'static`, for every `K: Hash + ?Sized` (unsized key types such as `str` and `[u8]`
/// included), hashing the key explicitly: build a
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
/// # A hand-written impl keeps the inherent lookups
///
/// A hand-written `ShardHasher` is the supported way to control shard routing, and it costs
/// nothing on the owned-key path. The six inherent lookups on every sharded store (`get`,
/// `remove`, `remove_entry`, `delete`, `contains`, `peek`) take a `&Q where K: Borrow<Q>` and
/// are bounded on `H: ShardHasher<Q>`. At `Q = K` that bound is exactly the one the store
/// already carries, so a router implementing `ShardHasher<K>` keeps all six: `cache.get(&key)`
/// compiles and routes through the custom `shard_hash`; no `.unwrap()`-returning trait call is
/// needed to reach it.
///
/// Borrowed keys stop at those six. Everything else on the sharded surface still takes `&K`:
/// the trait methods (`ConcurrentCached::cache_get` and friends, `ConcurrentCachedExt::get`) and
/// the expiry accessors (`peek_expires_at`, `expires_at`). A `String`-keyed store is looked up by
/// `&str` through `get`/`peek`/`contains`/`remove`/`remove_entry`/`delete` and by `&String`
/// everywhere else.
///
/// Borrowed-key lookups are the opt-in part. `cache.get(&q)` for some `Q` other than `K` needs
/// a *second* impl, `ShardHasher<Q>`, on the same router; without it that one call site does not
/// compile while the owned-key ones still do. Each impl a router adds is one more key type the
/// store can be looked up by, and coherence permits the whole set because a hand-written router
/// is deliberately not a `BuildHasher`.
///
/// Read the error for that missing second impl carefully. When the router implements exactly one
/// `ShardHasher<K>`, inference resolves `Q` to that single impl and rustc reports a type mismatch
/// rather than an unsatisfied bound -- `expected &UserId, found &u64` at the `cache.get(&7u64)`
/// argument, not "`TenantRouter: ShardHasher<u64>` is not satisfied". The fix is still to add
/// `impl ShardHasher<u64> for TenantRouter`.
///
/// ```rust
/// use cached::{ShardHasher, ShardedUnboundCache};
/// use std::borrow::Borrow;
///
/// #[derive(Hash, PartialEq, Eq)]
/// struct UserId(u64);
///
/// impl Borrow<u64> for UserId {
///     fn borrow(&self) -> &u64 {
///         &self.0
///     }
/// }
///
/// /// A random seed sampled once per process. Without it the routing is a published
/// /// function of the key and an attacker who picks the keys can put them all on one
/// /// shard; see the unkeyed-router warning below.
/// fn shard_seed() -> u64 {
///     use std::hash::{BuildHasher, Hasher};
///     static SEED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
///     *SEED.get_or_init(|| std::hash::RandomState::new().build_hasher().finish())
/// }
///
/// /// Deliberately not a `BuildHasher`: routing, not hashing, is the point.
/// #[derive(Clone)]
/// struct TenantRouter;
///
/// impl TenantRouter {
///     /// The one routing body both impls call, so they cannot disagree.
///     fn route(&self, raw: u64) -> u64 {
///         (raw ^ shard_seed()).wrapping_mul(0x9e3779b97f4a7c15)
///     }
/// }
///
/// impl ShardHasher<UserId> for TenantRouter {
///     fn shard_hash(&self, key: &UserId) -> u64 {
///         self.route(key.0)
///     }
/// }
///
/// // The opt-in second impl. It agrees with the first on keys that compare equal, which is
/// // the contract described below.
/// impl ShardHasher<u64> for TenantRouter {
///     fn shard_hash(&self, key: &u64) -> u64 {
///         self.route(*key)
///     }
/// }
///
/// let cache = ShardedUnboundCache::<UserId, u64>::builder()
///     .hasher(TenantRouter)
///     .build()
///     .unwrap();
/// cache.set(UserId(7), 70);
///
/// // Owned key: the inherent method, resolved through `ShardHasher<UserId>`.
/// assert_eq!(cache.get(&UserId(7)), Some(70));
/// // Borrowed key: the same entry, resolved through `ShardHasher<u64>`.
/// assert_eq!(cache.get(&7u64), Some(70));
/// ```
///
/// If shard routing is not the point and only the hash function is, pass a `BuildHasher`
/// (for example `std::hash::RandomState` or `ahash::RandomState`) to the builder's `hasher`
/// instead: the blanket impl covers every key type at once, so no per-`Q` impl is ever needed.
///
/// # Contract: a router's impls must agree with each other
///
/// If a type implements `ShardHasher` for more than one key type, all of those impls must agree
/// on keys that compare equal. Concretely, for `K: Borrow<Q>`, `shard_hash(&k)` must return the
/// same value as `shard_hash(k.borrow())`.
///
/// Violating this routes an owned insert and an equivalent borrowed lookup to different shards:
/// `get`/`peek`/`contains` report a miss on an entry the cache still holds, and
/// `remove`/`delete` silently no-op instead of removing it. There is no panic and no error, only
/// a cache that appears to lose entries it has. Only `get` bumps the `misses` counter while it
/// does so; `peek` and `contains` are peek-based and touch no counters at all, so
/// `metrics().misses` staying flat under those two probes is not evidence that routing agrees. This is the same species of
/// requirement as [`Borrow`](std::borrow::Borrow)'s own -- that equal keys hash equally -- and
/// the compiler cannot check either one.
///
/// Types reaching `ShardHasher` through the blanket [`BuildHasher`](std::hash::BuildHasher)
/// impl satisfy the contract automatically: one impl covers every key type, its body depends
/// only on the key's [`Hash`](std::hash::Hash) impl, and the `Borrow` contract already requires
/// those to agree. Only a router that hand-writes two or more impls can break it.
///
/// A guard test pins the property for one specific router. Substitute your own router, key type
/// and borrowed form, and paste the whole `#[test]` into your own test suite. Keep both halves:
/// the `assert_eq!` loop alone passes for a router whose impls both return a constant, which
/// agrees with itself while routing every key to shard 0, so the `assert_ne!` is what separates
/// "agrees" from "does nothing".
///
/// ```rust,test_harness
/// use cached::ShardHasher;
/// use std::borrow::Borrow;
///
/// #[derive(Hash, PartialEq, Eq)]
/// struct UserId(u64);
///
/// impl Borrow<u64> for UserId {
///     fn borrow(&self) -> &u64 {
///         &self.0
///     }
/// }
///
/// /// A random seed sampled once per process; see the unkeyed-router warning below.
/// fn shard_seed() -> u64 {
///     use std::hash::{BuildHasher, Hasher};
///     static SEED: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
///     *SEED.get_or_init(|| std::hash::RandomState::new().build_hasher().finish())
/// }
///
/// #[derive(Clone)]
/// struct TenantRouter;
///
/// impl TenantRouter {
///     fn route(&self, raw: u64) -> u64 {
///         (raw ^ shard_seed()).wrapping_mul(0x9e3779b97f4a7c15)
///     }
/// }
///
/// impl ShardHasher<UserId> for TenantRouter {
///     fn shard_hash(&self, key: &UserId) -> u64 {
///         self.route(key.0)
///     }
/// }
///
/// impl ShardHasher<u64> for TenantRouter {
///     fn shard_hash(&self, key: &u64) -> u64 {
///         self.route(*key)
///     }
/// }
///
/// #[test]
/// fn owned_and_borrowed_route_together() {
///     let router = TenantRouter;
///     // Every owned key must route exactly where its borrowed form routes.
///     for raw in [0u64, 1, 7, 0x9e37_79b9, u64::MAX] {
///         let owned = UserId(raw);
///         let borrowed: &u64 = owned.borrow();
///         assert_eq!(
///             ShardHasher::<UserId>::shard_hash(&router, &owned),
///             ShardHasher::<u64>::shard_hash(&router, borrowed),
///             "shard_hash disagrees between UserId({raw}) and its borrowed u64"
///         );
///     }
///     // And keys that are not equal must not all collapse onto one shard.
///     assert_ne!(
///         ShardHasher::<UserId>::shard_hash(&router, &UserId(7)),
///         ShardHasher::<u64>::shard_hash(&router, &8u64)
///     );
/// }
/// ```
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
/// # Warning: an unkeyed router is invertible
///
/// A router whose body is a fixed expression of the key (`key.wrapping_mul(CONST)`, a bare
/// multiply-shift, a prefix table) is a published function: the constant, the `>> 32` and the
/// shard mask are all in this documentation. An attacker who influences the keys (a tenant id
/// from a header, a request path, a user-supplied id) can solve offline for keys that all land
/// on one shard. The result is not a wrong answer but a collapse: every request serializes on
/// that shard's `RwLock` (a write lock even for reads, on the three LRU-bounded stores), and on
/// a bounded store the usable capacity drops to `max_size / shards` because the other shards
/// stay empty.
///
/// Mix in a random seed sampled once per process, as the examples above do:
///
/// ```rust
/// use std::hash::{BuildHasher, Hasher};
/// use std::sync::OnceLock;
///
/// fn shard_seed() -> u64 {
///     static SEED: OnceLock<u64> = OnceLock::new();
///     *SEED.get_or_init(|| std::hash::RandomState::new().build_hasher().finish())
/// }
///
/// // Keyed: the same multiply, over a value the attacker cannot predict.
/// fn route(raw: u64) -> u64 {
///     (raw ^ shard_seed()).wrapping_mul(0x9e3779b97f4a7c15)
/// }
/// # assert_ne!(route(7), route(8));
/// ```
///
/// The seed has to be shared by every impl on the router (route through one private helper, as
/// above) or the impls disagree and the cross-impl contract breaks. This warning does not apply
/// to the default path: [`DefaultShardHasher`] is randomly seeded per instance already, as is
/// any `BuildHasher` reaching `ShardHasher` through the blanket impl that seeds itself
/// (`std::hash::RandomState::new`, `ahash::RandomState::new`).
///
/// # Example
///
/// ```rust
/// use cached::ShardHasher;
///
/// /// Distributes `u64` keys using Fibonacci hashing (`key * 2^64/φ`).
/// /// Ensures the upper 32 bits (used for shard selection) are well-distributed.
/// ///
/// /// Unkeyed, so this is for keys you choose yourself. On attacker-influenced keys, xor in a
/// /// per-process random seed first (see the unkeyed-router warning above).
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
#[diagnostic::on_unimplemented(
    message = "`{Self}` cannot route keys of type `{K}` to a shard",
    label = "`{Self}` has no `ShardHasher<{K}>` impl",
    note = "a hand-written router implements `ShardHasher` once per key type it routes; on a concrete router type, add `impl ShardHasher<{K}> for {Self}`",
    note = "if `{Self}` is a type parameter of your own generic code it cannot carry an impl: add `{Self}: ShardHasher<{K}>` to that function's where clause, in addition to (not instead of) the store's own `ShardHasher<K>` bound",
    note = "borrowed-key lookups need their own impl: `cache.get(&q)` on a store keyed by `K` resolves through `ShardHasher<Q>`, not `ShardHasher<K>`",
    note = "every impl on one router must agree on keys that compare equal -- for `K: Borrow<Q>`, `shard_hash(&k)` must equal `shard_hash(k.borrow())`",
    note = "a `BuildHasher` that is `Clone + Send + Sync + 'static` covers every key type at once through the blanket impl and needs no per-key impl; that is an alternative only for a type carrying no hand-written `ShardHasher` impls, since coherence rejects both on one type"
)]
pub trait ShardHasher<K: ?Sized>: Clone + Send + Sync + 'static {
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
/// Routing builds a [`Hasher`](std::hash::Hasher) with
/// [`build_hasher`](std::hash::BuildHasher::build_hasher), feeds the key to it with
/// [`Hash::hash`](std::hash::Hash::hash), then [`finish`](std::hash::Hasher::finish)s it. Each
/// store's `shard_of_borrowed` reaches a borrowed key's shard by calling `shard_hash` itself, so
/// for a `BuildHasher` the owned and borrowed paths are not merely the same construction, they
/// are the same call, and an owned key and a borrowed key that compare equal cannot land on
/// different shards. This is deliberately not
/// [`BuildHasher::hash_one`](std::hash::BuildHasher::hash_one): that is an overridable
/// provided method whose implementation may dispatch on its static type argument (`ahash`'s does,
/// under nightly's `specialize` cfg), so `hash_one::<&K>` and `hash_one::<&Q>` are not required to
/// agree even when the two values hash identically. Building the `Hasher` explicitly depends only
/// on the `Hash` impl, which the `Borrow` contract already requires to agree.
///
/// The key parameter is `?Sized`, so unsized keys are covered too: a `String`-keyed store routes
/// a `&str` lookup through `ShardHasher<str>` on the very same hasher value.
impl<K, S> ShardHasher<K> for S
where
    K: std::hash::Hash + ?Sized,
    S: std::hash::BuildHasher + Clone + Send + Sync + 'static,
{
    fn shard_hash(&self, key: &K) -> u64 {
        routing_hash(self, key)
    }
}

/// Default shard hasher backed by `ahash::RandomState` (or `std::collections::hash_map::RandomState`
/// when the `ahash` feature is disabled).
///
/// It implements [`BuildHasher`](std::hash::BuildHasher) and picks up
/// [`ShardHasher<K>`] for every `K: Hash + ?Sized` through the blanket impl above, so it is also a valid
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

/// Do not override [`hash_one`](std::hash::BuildHasher::hash_one) here, however tempting it is to
/// forward it to the inner `ahash::RandomState` for its specialized path. `hash_one` may dispatch
/// on its static type argument, and ahash's does under nightly's `specialize` cfg: it specializes
/// some reference types (`&u8`..`&i64`, `&u128`/`&i128`/`&usize`/`&isize`) and not others
/// (`&str`, `&String`, `&[u8]`), so `hash_one::<&K>` and `hash_one::<&Q>` are not required to
/// agree even when the two values hash identically. With an override in place, a newtype key such
/// as `struct UserId(u64)` with `Borrow<u64>` routes its owned inserts and its borrowed lookups to
/// different shards on any nightly compiler, with no panic and no error. Leaving `hash_one`
/// un-overridden keeps it equal to the provided body, which is the same construction as
/// `routing_hash` above. `default_shard_hasher_routes_through_build_hasher` in this module's tests
/// pins the equality.
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
    /// override `hash_one`, so the two agree here. Coherence still makes the blanket impl
    /// exclusive after the relaxation to `K: ?Sized`: a second, explicit
    /// `ShardHasher for DefaultShardHasher` impl alongside the blanket one would be a
    /// duplicate-impl error, so this compiling is itself the single-path check.
    #[test]
    fn default_shard_hasher_routes_through_build_hasher() {
        use std::hash::BuildHasher;
        let h = DefaultShardHasher::new();
        assert_eq!(h.shard_hash(&42u64), h.hash_one(42u64));
        assert_eq!(h.shard_hash(&"key"), h.hash_one("key"));
    }

    /// The blanket impl covers unsized key types, which is what lets a borrowed lookup route
    /// through `ShardHasher<Q>` (`Q = str` here) rather than bypassing the trait. Before
    /// `ShardHasher<K: ?Sized>` this did not compile at all, and the value it produces must
    /// match the owned key's routing or the entry becomes unreachable through its borrowed form.
    #[test]
    fn blanket_impl_covers_unsized_keys() {
        let h = DefaultShardHasher::new();
        let owned = String::from("key");
        let borrowed: &str = std::borrow::Borrow::borrow(&owned);
        assert_eq!(
            ShardHasher::<String>::shard_hash(&h, &owned),
            ShardHasher::<str>::shard_hash(&h, borrowed)
        );
        assert_ne!(
            ShardHasher::<str>::shard_hash(&h, borrowed),
            ShardHasher::<str>::shard_hash(&h, "other")
        );
    }

    /// One router type carries a hand-written `ShardHasher` impl per key type it routes, and
    /// both impls are reachable from the same value. Coherence permits the set because
    /// `TwoWayRouter` is deliberately not a `BuildHasher`, so nothing overlaps the blanket impl.
    /// This is the shape a store uses to serve an owned `get(&UserId(..))` through
    /// `ShardHasher<UserId>` and a borrowed `get(&7u64)` through `ShardHasher<u64>`.
    #[test]
    fn hand_written_router_carries_one_impl_per_key_type() {
        use std::borrow::Borrow;

        #[derive(Hash, PartialEq, Eq)]
        struct UserId(u64);

        impl Borrow<u64> for UserId {
            fn borrow(&self) -> &u64 {
                &self.0
            }
        }

        #[derive(Clone)]
        struct TwoWayRouter;

        impl ShardHasher<UserId> for TwoWayRouter {
            fn shard_hash(&self, key: &UserId) -> u64 {
                key.0.wrapping_mul(0x9e37_79b9_7f4a_7c15)
            }
        }

        impl ShardHasher<u64> for TwoWayRouter {
            fn shard_hash(&self, key: &u64) -> u64 {
                key.wrapping_mul(0x9e37_79b9_7f4a_7c15)
            }
        }

        let router = TwoWayRouter;
        for raw in [0u64, 1, 7, 0x9e37_79b9, u64::MAX] {
            let owned = UserId(raw);
            let borrowed: &u64 = owned.borrow();
            // Both impls resolve off the one value, and they agree: the cross-impl
            // consistency contract documented on `ShardHasher`.
            assert_eq!(
                ShardHasher::<UserId>::shard_hash(&router, &owned),
                ShardHasher::<u64>::shard_hash(&router, borrowed),
                "shard_hash disagrees between UserId({raw}) and its borrowed u64"
            );
        }
        assert_ne!(
            ShardHasher::<UserId>::shard_hash(&router, &UserId(7)),
            ShardHasher::<u64>::shard_hash(&router, &8u64)
        );
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
