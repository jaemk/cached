/*!
Cross-store certification for `CacheExpiry`/`ConcurrentCacheExpiry` -- both the value-returning
`cache_peek_expires_at` and the value-free `cache_expires_at` (issue #91).

Every implementing store already carries thorough in-file unit tests (named `peek_expires_at_*`)
in its own module under `src/stores`, pinning that store's own override against the
four-quadrant contract in isolation. What no single in-file test can show is that the SAME contract holds *uniformly*
across all nine implementors. This file exists for that: every property below is expressed once
as a generic function bound only by `CacheExpiry<K, V>` / `ConcurrentCacheExpiry<K, V>` (plus
whatever companion trait the property needs), then invoked with the identical body against every
applicable concrete store. A store whose override diverges from its siblings fails the shared
assertion, not a store-specific copy of it.

The file also only ever reaches the traits through `cached::prelude::*` (never a direct
`use cached::CacheExpiry`), so an export regression fails every test here, not just a dedicated
reachability check.

Properties certified, each for BOTH reads:

  1. The four-quadrant return table -- `(None, None)` absent, `(Some(v), None)` present with no
     known deadline, `(Some(v), Some(future))` live, `(Some(v), Some(past))` expired-but-not-removed
     -- uniformly across all nine stores. `cache_expires_at` reports the same four cases with a
     `bool` presence flag in place of the value: `(false, None)`, `(true, None)`,
     `(true, Some(future))`, `(true, Some(past))`.
  2. Side-effect freedom, uniformly: hits/misses counters, entry count, and eviction count are
     unchanged by a read of a present AND an absent key, and a read of the LRU-tail key on an
     LRU-backed store does not save it from the next capacity eviction (i.e. does not promote
     recency).
  3. Agreement with `cache_peek_with_expiry_status`: on the TTL-based stores the returned deadline
     is in the past exactly when that method reports `true`.
  4. The `Expires`-store caveat, asserted rather than assumed: on the four `Expires`-based stores
     the deadline is advisory (from `Expires::expires_at`, default `None`), so an expired value
     that does not override `expires_at` reports `(Some(v), None)` / `(true, None)` -- `None` must
     never be read as proof of liveness, and the presence flag is independent of expiry.
     `is_expired` (via `cache_peek_with_expiry_status`) remains the authority.
  5. Reachability through `cached::prelude::*` and through a generic `T: CacheExpiry<K, V>` /
     `T: ConcurrentCacheExpiry<K, V>` bound.
  6. The `K: Borrow<Q>` generality of the single-owner trait: a `String`-keyed store read with
     a `&str` (`Q = str`, so `Q != K`). Every other call site in this file and in the per-store
     unit tests passes `Q == K`, which leaves the one bound that distinguishes `CacheExpiry`
     from `ConcurrentCacheExpiry` unexercised. `ConcurrentCacheExpiry` takes `&K` by design, so
     there is no concurrent counterpart to certify.
  7. Agreement BETWEEN the two reads, which is what keeps them from drifting apart as separate
     store overrides: for the same key on the same store, `cache_expires_at` reports the identical
     deadline `cache_peek_expires_at` does, and its presence flag is `true` exactly when the peek
     returns `Some(v)`. Checked in both call orders, so neither read perturbs what the other sees,
     and against the `expires_at` / `peek_expires_at` aliases so a defaulted alias cannot diverge
     from the required method underneath it.
  8. The absent `V: Clone` bound, which is the whole reason `cache_expires_at` exists next to the
     peek: a deadline is readable for a value type that does not implement `Clone` at all. Every
     value-free helper below is generic over `V` with no `V: Clone` bound, so those signatures are
     themselves the assertion, and they are instantiated with deliberately non-`Clone` value types.

Also certified where the store supports it: no deadline movement across two peeks with
`set_refresh_on_hit(true)` (TTL stores only -- `TtlSortedCache` deliberately has no
refresh-on-hit mode at all, so it is excluded from that one property).

Feature gating: the TTL stores (`TtlCache`, `LruTtlCache`, `TtlSortedCache`, `ShardedTtlCache`,
`ShardedLruTtlCache`) require the `time_stores` feature, but the four `Expires`-based stores
(`ExpiringCache`, `ExpiringLruCache`, `ShardedExpiringCache`, `ShardedExpiringLruCache`) are
ungated. So gating is per-module, not a blanket `#![cfg(...)]` on the whole file: the
`expires_stores` module runs under every feature configuration, including zero features.
*/

use cached::prelude::*;
use cached::time::{Duration, Instant};
use std::hash::Hash;

// ── Shared fixture: the `Expires`-based stores' value type ────────────────────────────────────
//
// `is_expired` is driven by an explicit flag (deterministic, no sleeps needed), and `expires_at`
// is overridden only when a deadline is supplied. The same type produces every quadrant the
// `Expires`-store tests need, including the advisory-caveat shape: expired with no override.

#[derive(Clone, Debug, PartialEq)]
struct AdvisoryToken {
    payload: i32,
    expired: bool,
    deadline: Option<Instant>,
}

impl AdvisoryToken {
    fn live(payload: i32) -> Self {
        AdvisoryToken {
            payload,
            expired: false,
            deadline: None,
        }
    }

    fn live_with_deadline(payload: i32, deadline: Instant) -> Self {
        AdvisoryToken {
            payload,
            expired: false,
            deadline: Some(deadline),
        }
    }

    fn stale_no_deadline(payload: i32) -> Self {
        AdvisoryToken {
            payload,
            expired: true,
            deadline: None,
        }
    }

    fn stale_with_deadline(payload: i32, deadline: Instant) -> Self {
        AdvisoryToken {
            payload,
            expired: true,
            deadline: Some(deadline),
        }
    }
}

impl Expires for AdvisoryToken {
    fn is_expired(&self) -> bool {
        self.expired
    }

    fn expires_at(&self) -> Option<Instant> {
        self.deadline
    }
}

// ── Shared fixture: value types that deliberately do NOT implement `Clone` ────────────────────
//
// `cache_peek_expires_at` hands the value back, so it carries a `V: Clone` bound and cannot be
// called for these at all. `cache_expires_at` never touches the value, carries no such bound, and
// must therefore work here. Neither of these types derives (or hand-writes) `Clone`: adding one
// would make the property-8 tests pass vacuously, so leave them non-`Clone`. The TTL family's
// counterpart, `ttl_stores::NotClone`, lives inside that (feature-gated) module.

/// A non-`Clone` value for the `Expires`-based stores, which require `V: Expires`. `Expires`
/// itself does not require `Clone`, so this is a legal value type for those stores.
#[derive(Debug, PartialEq)]
struct NotCloneToken {
    expired: bool,
    deadline: Option<Instant>,
}

impl Expires for NotCloneToken {
    fn is_expired(&self) -> bool {
        self.expired
    }

    fn expires_at(&self) -> Option<Instant> {
        self.deadline
    }
}

// ── Shared, family-agnostic generic helpers ────────────────────────────────────────────────────
//
// These are bound only by the common `Cached`/`ConcurrentCached` + `CacheExpiry`/
// `ConcurrentCacheExpiry` surface, so the identical body below is exercised against both the
// TTL-based and the `Expires`-based stores: the property genuinely holds crate-wide, not just
// within one store family.

/// Hits/misses counters, entry count, and eviction count are all identical before and after a
/// peek of a present key AND an absent key. The entry-count and eviction-counter checks matter on
/// their own: they are the only way this file would catch a peek that physically swept some
/// OTHER (unrelated) expired entry, or lazily removed the peeked one -- a mutation neither
/// counter alone would necessarily surface, and distinct from the "the peeked entry itself
/// survives" assertions the four-quadrant tests already make.
fn assert_side_effect_free_single_owner<K, V, C>(store: &mut C, present_key: &K, absent_key: &K)
where
    K: Hash + Eq,
    V: Clone,
    C: Cached<K, V> + CacheExpiry<K, V>,
{
    let hits0 = store.cache_hits();
    let misses0 = store.cache_misses();
    let size0 = store.cache_size();
    let evictions0 = store.cache_evictions();

    let _ = store.cache_peek_expires_at(present_key);
    let _ = store.cache_peek_expires_at(absent_key);
    let _ = store.cache_peek_expires_at(present_key);

    assert_eq!(
        store.cache_hits(),
        hits0,
        "a peek must not change the hit counter"
    );
    assert_eq!(
        store.cache_misses(),
        misses0,
        "a peek must not change the miss counter"
    );
    assert_eq!(
        store.cache_size(),
        size0,
        "a peek must not change the entry count (no lazy removal of the peeked or any other entry)"
    );
    assert_eq!(
        store.cache_evictions(),
        evictions0,
        "a peek must not increment the eviction counter"
    );
}

/// Concurrent counterpart of [`assert_side_effect_free_single_owner`].
fn assert_side_effect_free_concurrent<K, V, C>(store: &C, present_key: &K, absent_key: &K)
where
    V: Clone,
    C: ConcurrentCached<K, V> + ConcurrentCacheExpiry<K, V>,
{
    let hits0 = store.cache_hits();
    let misses0 = store.cache_misses();
    let size0 = store.cache_size().unwrap();
    let evictions0 = store.cache_evictions();

    let _ = store.cache_peek_expires_at(present_key);
    let _ = store.cache_peek_expires_at(absent_key);
    let _ = store.cache_peek_expires_at(present_key);

    assert_eq!(
        store.cache_hits(),
        hits0,
        "a peek must not change the hit counter"
    );
    assert_eq!(
        store.cache_misses(),
        misses0,
        "a peek must not change the miss counter"
    );
    assert_eq!(
        store.cache_size().unwrap(),
        size0,
        "a peek must not change the entry count (no lazy removal of the peeked or any other entry)"
    );
    assert_eq!(
        store.cache_evictions(),
        evictions0,
        "a peek must not increment the eviction counter"
    );
}

/// A peek of the LRU-tail key must not promote its recency: with `max_size` 2, inserting `k1`
/// (LRU) then `k2` (MRU), peeking `k1`, then inserting `k3` must evict `k1` -- the still-LRU key
/// -- not `k2`. If the peek had promoted `k1`, `k2` would be evicted instead.
fn assert_peek_does_not_promote_lru_single_owner<K, V, C>(
    mut store: C,
    k1: K,
    v1: V,
    k2: K,
    v2: V,
    k3: K,
    v3: V,
) where
    K: Clone + Hash + Eq,
    V: Clone + PartialEq + std::fmt::Debug,
    C: Cached<K, V> + CacheExpiry<K, V>,
{
    store.cache_set(k1.clone(), v1);
    store.cache_set(k2.clone(), v2.clone());

    // Peek the LRU key (k1). Must not promote it.
    let _ = store.cache_peek_expires_at(&k1);

    // Overflow: k3 must evict the still-LRU key (k1), not k2.
    store.cache_set(k3.clone(), v3.clone());

    assert_eq!(
        store.cache_get(&k1),
        None,
        "peek must not have promoted the LRU key; it must still be evicted"
    );
    assert_eq!(store.cache_get(&k2), Some(&v2));
    assert_eq!(store.cache_get(&k3), Some(&v3));
}

/// Concurrent counterpart of [`assert_peek_does_not_promote_lru_single_owner`].
fn assert_peek_does_not_promote_lru_concurrent<K, V, C>(
    store: C,
    k1: K,
    v1: V,
    k2: K,
    v2: V,
    k3: K,
    v3: V,
) where
    K: Clone,
    V: Clone + PartialEq + std::fmt::Debug,
    C: ConcurrentCached<K, V> + ConcurrentCacheExpiry<K, V>,
{
    let _ = store.cache_set(k1.clone(), v1).unwrap();
    let _ = store.cache_set(k2.clone(), v2.clone()).unwrap();

    let _ = store.cache_peek_expires_at(&k1);

    let _ = store.cache_set(k3.clone(), v3.clone()).unwrap();

    assert_eq!(
        store.cache_get(&k1).unwrap(),
        None,
        "peek must not have promoted the LRU key; it must still be evicted"
    );
    assert_eq!(store.cache_get(&k2).unwrap(), Some(v2));
    assert_eq!(store.cache_get(&k3).unwrap(), Some(v3));
}

/// Reachability through a bare generic bound: proves `CacheExpiry` is callable through nothing
/// but `T: CacheExpiry<K, V>`, independent of the concrete store type.
fn peek_via_generic_bound<K, V, C>(store: &C, key: &K) -> (Option<V>, Option<Instant>)
where
    K: Hash + Eq,
    V: Clone,
    C: CacheExpiry<K, V>,
{
    store.cache_peek_expires_at(key)
}

/// Concurrent counterpart of [`peek_via_generic_bound`].
fn peek_via_generic_bound_concurrent<K, V, C>(store: &C, key: &K) -> (Option<V>, Option<Instant>)
where
    V: Clone,
    C: ConcurrentCacheExpiry<K, V>,
{
    store.cache_peek_expires_at(key)
}

/// Peeks a `String`-keyed store through a BORROWED key, i.e. `Q = str` with `Q != K`.
///
/// This is the only property in this file that exercises the generic `Q` at all: every other
/// call site here (and everywhere else in the repo) passes `Q == K`, so the `K: Borrow<Q>`
/// bound that distinguishes [`CacheExpiry`] from [`ConcurrentCacheExpiry`] would be
/// unexercised without it. The signature is the assertion: this function cannot compile if
/// `cache_peek_expires_at` ever narrows to `&K`, because `&str` is not a `&String`. It also
/// removes the need for callers to allocate a `String` just to look one up.
///
/// There is deliberately no concurrent counterpart: [`ConcurrentCacheExpiry`] takes `&K`
/// (the concurrent family includes external stores that must serialize the key, and
/// `Borrow<Q>` carries no serialization guarantee), so no borrowed-key path exists there.
fn peek_via_borrowed_str<V, C>(store: &C, key: &str) -> (Option<V>, Option<Instant>)
where
    V: Clone,
    C: CacheExpiry<String, V>,
{
    store.cache_peek_expires_at(key)
}

/// A second, unrelated `Borrow<Q>` shape from the `&str`/`String` one above: `K = Vec<u8>`
/// peeked through `Q = [u8]`. `String`/`str` is the one `Borrow` pair the crate's own macros and
/// examples ever reach for, so a regression that special-cased that pair specifically (e.g. a
/// accidental `&str`-only overload, or a bound that reads `K: Borrow<str>` instead of the fully
/// generic `K: Borrow<Q>`) would slip past every other test in this file. `Vec<u8>` borrowing to
/// `[u8]` exercises the same generic bound through a completely different concrete `Q`.
fn peek_via_borrowed_slice<V, C>(store: &C, key: &[u8]) -> (Option<V>, Option<Instant>)
where
    V: Clone,
    C: CacheExpiry<Vec<u8>, V>,
{
    store.cache_peek_expires_at(key)
}

// ── The value-free read: shared, family-agnostic generic helpers ───────────────────────────────
//
// Counterparts of the helpers above for `cache_expires_at`. Note what is MISSING from every
// signature here: `V: Clone`. That absence is load-bearing, not incidental -- it is the reason the
// method exists next to `cache_peek_expires_at` -- so these helpers double as the compile-time
// proof of property 8, and are instantiated with the non-`Clone` fixtures above.

/// Property 7, the centerpiece: the two reads must not drift apart.
///
/// For one key on one store, `cache_expires_at` must report the identical deadline
/// `cache_peek_expires_at` does, and its presence flag must be `true` exactly when the peek
/// returns `Some(v)`. Both call orders are checked, so a read that perturbed what the other sees
/// (a lazy sweep, a recency bump) would fail here as well as in the side-effect tests. The
/// defaulted aliases are checked against their required methods too: `expires_at` and
/// `peek_expires_at` are overridable, so a store could in principle diverge there alone.
///
/// Called for a key in every quadrant, including absent, by the per-family drivers below.
fn assert_reads_agree_single_owner<K, V, C>(store: &C, key: &K)
where
    K: Hash + Eq,
    V: Clone,
    C: CacheExpiry<K, V>,
{
    let (value, peek_deadline) = store.cache_peek_expires_at(key);
    let (present, deadline) = store.cache_expires_at(key);
    assert_eq!(
        present,
        value.is_some(),
        "the presence flag must be true exactly when the peek returns a value"
    );
    assert_eq!(
        deadline, peek_deadline,
        "both reads must report the same deadline for the same key"
    );

    // The reverse order: neither read may change what the other then observes.
    let (present_first, deadline_first) = store.cache_expires_at(key);
    let (value_after, peek_deadline_after) = store.cache_peek_expires_at(key);
    assert_eq!(
        present_first,
        value_after.is_some(),
        "the two reads must still agree when the value-free one runs first"
    );
    assert_eq!(
        deadline_first, peek_deadline_after,
        "the two reads must still agree when the value-free one runs first"
    );
    assert_eq!(
        (present_first, deadline_first),
        (present, deadline),
        "a read must not change what a repeat of the same read reports"
    );

    // The defaulted aliases must not diverge from the required methods.
    assert_eq!(
        store.expires_at(key),
        (present, deadline),
        "the expires_at alias must agree with cache_expires_at"
    );
    assert_eq!(
        store.peek_expires_at(key).1,
        peek_deadline,
        "the peek_expires_at alias must agree with cache_peek_expires_at"
    );
}

/// Concurrent counterpart of [`assert_reads_agree_single_owner`].
fn assert_reads_agree_concurrent<K, V, C>(store: &C, key: &K)
where
    V: Clone,
    C: ConcurrentCacheExpiry<K, V>,
{
    let (value, peek_deadline) = store.cache_peek_expires_at(key);
    let (present, deadline) = store.cache_expires_at(key);
    assert_eq!(
        present,
        value.is_some(),
        "the presence flag must be true exactly when the peek returns a value"
    );
    assert_eq!(
        deadline, peek_deadline,
        "both reads must report the same deadline for the same key"
    );

    let (present_first, deadline_first) = store.cache_expires_at(key);
    let (value_after, peek_deadline_after) = store.cache_peek_expires_at(key);
    assert_eq!(
        present_first,
        value_after.is_some(),
        "the two reads must still agree when the value-free one runs first"
    );
    assert_eq!(
        deadline_first, peek_deadline_after,
        "the two reads must still agree when the value-free one runs first"
    );
    assert_eq!(
        (present_first, deadline_first),
        (present, deadline),
        "a read must not change what a repeat of the same read reports"
    );

    assert_eq!(
        store.expires_at(key),
        (present, deadline),
        "the expires_at alias must agree with cache_expires_at"
    );
    assert_eq!(
        store.peek_expires_at(key).1,
        peek_deadline,
        "the peek_expires_at alias must agree with cache_peek_expires_at"
    );
}

/// Value-free counterpart of [`assert_side_effect_free_single_owner`]: hits/misses counters, entry
/// count, and eviction count are all identical before and after a `cache_expires_at` of a present
/// key AND an absent key.
fn assert_expires_at_side_effect_free_single_owner<K, V, C>(
    store: &mut C,
    present_key: &K,
    absent_key: &K,
) where
    K: Hash + Eq,
    C: Cached<K, V> + CacheExpiry<K, V>,
{
    let hits0 = store.cache_hits();
    let misses0 = store.cache_misses();
    let size0 = store.cache_size();
    let evictions0 = store.cache_evictions();

    let _ = store.cache_expires_at(present_key);
    let _ = store.cache_expires_at(absent_key);
    let _ = store.cache_expires_at(present_key);

    assert_eq!(
        store.cache_hits(),
        hits0,
        "a value-free deadline read must not change the hit counter"
    );
    assert_eq!(
        store.cache_misses(),
        misses0,
        "a value-free deadline read must not change the miss counter"
    );
    assert_eq!(
        store.cache_size(),
        size0,
        "a value-free deadline read must not change the entry count (no lazy removal of the read \
         or any other entry)"
    );
    assert_eq!(
        store.cache_evictions(),
        evictions0,
        "a value-free deadline read must not increment the eviction counter"
    );
}

/// Concurrent counterpart of [`assert_expires_at_side_effect_free_single_owner`].
fn assert_expires_at_side_effect_free_concurrent<K, V, C>(
    store: &C,
    present_key: &K,
    absent_key: &K,
) where
    C: ConcurrentCached<K, V> + ConcurrentCacheExpiry<K, V>,
{
    let hits0 = store.cache_hits();
    let misses0 = store.cache_misses();
    let size0 = store.cache_size().unwrap();
    let evictions0 = store.cache_evictions();

    let _ = store.cache_expires_at(present_key);
    let _ = store.cache_expires_at(absent_key);
    let _ = store.cache_expires_at(present_key);

    assert_eq!(
        store.cache_hits(),
        hits0,
        "a value-free deadline read must not change the hit counter"
    );
    assert_eq!(
        store.cache_misses(),
        misses0,
        "a value-free deadline read must not change the miss counter"
    );
    assert_eq!(
        store.cache_size().unwrap(),
        size0,
        "a value-free deadline read must not change the entry count (no lazy removal of the read \
         or any other entry)"
    );
    assert_eq!(
        store.cache_evictions(),
        evictions0,
        "a value-free deadline read must not increment the eviction counter"
    );
}

/// Value-free counterpart of [`assert_peek_does_not_promote_lru_single_owner`], asserted the same
/// behavioural way rather than by listing an internal order: with `max_size` 2, inserting `k1`
/// (LRU) then `k2` (MRU), reading `k1`'s deadline, then inserting `k3` must still evict `k1`. A
/// read that promoted `k1` would make `k2` the victim instead.
fn assert_expires_at_does_not_promote_lru_single_owner<K, V, C>(
    mut store: C,
    k1: K,
    v1: V,
    k2: K,
    v2: V,
    k3: K,
    v3: V,
) where
    K: Clone + Hash + Eq,
    V: PartialEq + std::fmt::Debug,
    C: Cached<K, V> + CacheExpiry<K, V>,
{
    store.cache_set(k1.clone(), v1);
    store.cache_set(k2.clone(), v2);

    // Read the LRU key's deadline (k1). Must not promote it. Repeated, so a store that promoted
    // only on a second read cannot slip through.
    for _ in 0..3 {
        let (present, _) = store.cache_expires_at(&k1);
        assert!(
            present,
            "setup: the key being read must actually be present, or the property is vacuous"
        );
    }

    // Overflow: k3 must evict the still-LRU key (k1), not k2.
    store.cache_set(k3.clone(), v3);

    assert_eq!(
        store.cache_get(&k1),
        None,
        "a value-free deadline read must not have promoted the LRU key; it must still be evicted"
    );
    assert!(store.cache_get(&k2).is_some());
    assert!(store.cache_get(&k3).is_some());
}

/// Concurrent counterpart of [`assert_expires_at_does_not_promote_lru_single_owner`].
///
/// `V: Clone` reappears here only because the concurrent stores' own `ConcurrentCached` impls
/// require it to insert and to read a value back at all; the deadline read itself still does not.
fn assert_expires_at_does_not_promote_lru_concurrent<K, V, C>(
    store: C,
    k1: K,
    v1: V,
    k2: K,
    v2: V,
    k3: K,
    v3: V,
) where
    K: Clone,
    V: Clone + PartialEq + std::fmt::Debug,
    C: ConcurrentCached<K, V> + ConcurrentCacheExpiry<K, V>,
{
    let _ = store.cache_set(k1.clone(), v1).unwrap();
    let _ = store.cache_set(k2.clone(), v2.clone()).unwrap();

    for _ in 0..3 {
        let (present, _) = store.cache_expires_at(&k1);
        assert!(
            present,
            "setup: the key being read must actually be present, or the property is vacuous"
        );
    }

    let _ = store.cache_set(k3.clone(), v3.clone()).unwrap();

    assert_eq!(
        store.cache_get(&k1).unwrap(),
        None,
        "a value-free deadline read must not have promoted the LRU key; it must still be evicted"
    );
    assert_eq!(store.cache_get(&k2).unwrap(), Some(v2));
    assert_eq!(store.cache_get(&k3).unwrap(), Some(v3));
}

/// Reachability through a bare generic bound, and simultaneously the proof that `cache_expires_at`
/// carries no `V: Clone` bound: this function is callable for a `V` that is not `Clone`, which
/// [`peek_via_generic_bound`] is not. The signature is the assertion.
fn expires_at_via_generic_bound<K, V, C>(store: &C, key: &K) -> (bool, Option<Instant>)
where
    K: Hash + Eq,
    C: CacheExpiry<K, V>,
{
    store.cache_expires_at(key)
}

/// Concurrent counterpart of [`expires_at_via_generic_bound`], likewise with no `V: Clone`.
fn expires_at_via_generic_bound_concurrent<K, V, C>(store: &C, key: &K) -> (bool, Option<Instant>)
where
    C: ConcurrentCacheExpiry<K, V>,
{
    store.cache_expires_at(key)
}

/// Value-free counterpart of [`peek_via_borrowed_str`]: `K = String` read through `Q = str`.
/// Cannot compile if `cache_expires_at` ever narrows to `&K`.
fn expires_at_via_borrowed_str<V, C>(store: &C, key: &str) -> (bool, Option<Instant>)
where
    C: CacheExpiry<String, V>,
{
    store.cache_expires_at(key)
}

/// Value-free counterpart of [`peek_via_borrowed_slice`]: `K = Vec<u8>` read through `Q = [u8]`,
/// a `Borrow` pair unrelated to `String`/`str`. See [`peek_via_borrowed_slice`] for why the second
/// shape is needed.
fn expires_at_via_borrowed_slice<V, C>(store: &C, key: &[u8]) -> (bool, Option<Instant>)
where
    C: CacheExpiry<Vec<u8>, V>,
{
    store.cache_expires_at(key)
}

// ── TTL-based stores: TtlCache, LruTtlCache, TtlSortedCache, ShardedTtlCache, ────────────────
// ── ShardedLruTtlCache. Real deadlines; require `time_stores`. ────────────────────────────────

mod ttl_stores {
    #![cfg(feature = "time_stores")]

    use super::*;
    use cached::{LruTtlCache, ShardedLruTtlCache, ShardedTtlCache, TtlCache, TtlSortedCache};

    // A short-but-nonzero TTL the entry outlives within a "live" check, then a sleep past it to
    // force expiry. Kept small to keep the suite fast while staying deterministic.
    const SHORT_TTL: Duration = Duration::from_millis(80);
    const PAST_TTL: Duration = Duration::from_millis(160);
    // Used by tests that are not about expiry timing at all (side effects, LRU, reachability),
    // so there is no risk of an incidental expiry mid-test.
    const LONG_TTL: Duration = Duration::from_secs(5);

    /// The TTL family's non-`Clone` value type; see the shared fixture comment above
    /// [`NotCloneToken`]. The TTL stores put no bound on `V` at all, so a bare tuple struct with
    /// no `Clone` is a legal value type for them. Do not add `Clone`: its absence is the point.
    #[derive(Debug, PartialEq)]
    pub struct NotClone(pub i32);

    /// The four-quadrant `CacheExpiry` contract, generic over any single-owner TTL store that
    /// exposes nothing but the common `Cached` + `CacheTtl` + `CacheExpiry` surface. Invoked with
    /// the identical body against all three single-owner TTL stores below.
    fn assert_four_quadrants<C>(mut store: C)
    where
        C: Cached<u32, i32> + CacheTtl + CacheExpiry<u32, i32>,
    {
        // (None, None): absent key.
        assert_eq!(store.cache_peek_expires_at(&404u32), (None, None));

        // (Some(v), None): present, no known deadline (ttl disabled at insert time).
        let original_ttl = store.ttl();
        store.unset_ttl();
        store.cache_set(1, 100);
        assert_eq!(store.cache_peek_expires_at(&1u32), (Some(100), None));
        if let Some(ttl) = original_ttl {
            store.set_ttl(ttl);
        }

        // (Some(v), Some(future)): live.
        store.cache_set(2, 200);
        let (value, deadline) = store.cache_peek_expires_at(&2u32);
        assert_eq!(value, Some(200));
        assert!(
            deadline.is_some_and(|t| t > Instant::now()),
            "a live entry's deadline must be in the future"
        );

        // (Some(v), Some(past)): expired, not removed.
        store.cache_set(3, 300);
        std::thread::sleep(PAST_TTL);
        let (value, deadline) = store.cache_peek_expires_at(&3u32);
        assert_eq!(value, Some(300));
        assert!(
            deadline.is_some_and(|t| t <= Instant::now()),
            "an expired entry's deadline must be in the past"
        );
        assert_eq!(
            store.cache_peek_expires_at(&3u32),
            (Some(300), deadline),
            "an expired entry must survive the peek"
        );
    }

    /// Concurrent counterpart of [`assert_four_quadrants`].
    fn assert_four_quadrants_concurrent<C>(store: C)
    where
        C: ConcurrentCached<u32, i32> + ConcurrentCacheTtl + ConcurrentCacheExpiry<u32, i32>,
    {
        assert_eq!(store.cache_peek_expires_at(&404u32), (None, None));

        let original_ttl = store.ttl();
        store.unset_ttl();
        let _ = store.cache_set(1, 100).unwrap();
        assert_eq!(store.cache_peek_expires_at(&1u32), (Some(100), None));
        if let Some(ttl) = original_ttl {
            store.set_ttl(ttl);
        }

        let _ = store.cache_set(2, 200).unwrap();
        let (value, deadline) = store.cache_peek_expires_at(&2u32);
        assert_eq!(value, Some(200));
        assert!(
            deadline.is_some_and(|t| t > Instant::now()),
            "a live entry's deadline must be in the future"
        );

        let _ = store.cache_set(3, 300).unwrap();
        std::thread::sleep(PAST_TTL);
        let (value, deadline) = store.cache_peek_expires_at(&3u32);
        assert_eq!(value, Some(300));
        assert!(
            deadline.is_some_and(|t| t <= Instant::now()),
            "an expired entry's deadline must be in the past"
        );
        assert_eq!(
            store.cache_peek_expires_at(&3u32),
            (Some(300), deadline),
            "an expired entry must survive the peek"
        );
    }

    #[test]
    fn four_quadrants_uniform_across_single_owner_ttl_stores() {
        assert_four_quadrants(
            TtlCache::<u32, i32>::builder()
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
        assert_four_quadrants(
            LruTtlCache::<u32, i32>::builder()
                .max_size(8)
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
        assert_four_quadrants(
            TtlSortedCache::<u32, i32>::builder()
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
    }

    #[test]
    fn four_quadrants_uniform_across_sharded_ttl_stores() {
        assert_four_quadrants_concurrent(
            ShardedTtlCache::<u32, i32>::builder()
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
        assert_four_quadrants_concurrent(
            ShardedLruTtlCache::<u32, i32>::builder()
                .shards(1)
                .per_shard_max_size(8)
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
    }

    /// No deadline movement across two peeks with `set_refresh_on_hit(true)`. `TtlSortedCache`
    /// deliberately does not implement `CacheRefreshOnHit` (its entries are deadline-ordered), so
    /// it is excluded here by construction (the bound would not be satisfiable).
    fn assert_peek_does_not_renew<C>(mut store: C)
    where
        C: Cached<u32, i32> + CacheExpiry<u32, i32> + CacheRefreshOnHit,
    {
        store.set_refresh_on_hit(true);
        store.cache_set(1, 100);

        let (_, first) = store.cache_peek_expires_at(&1u32);
        std::thread::sleep(Duration::from_millis(40));
        let (_, second) = store.cache_peek_expires_at(&1u32);
        assert_eq!(
            first, second,
            "peeking must not renew the ttl even with refresh_on_hit enabled"
        );

        // Control: a real read DOES renew, proving the assertion above is not vacuous.
        assert_eq!(store.cache_get(&1u32), Some(&100));
        let (_, after_hit) = store.cache_peek_expires_at(&1u32);
        assert!(
            after_hit > first,
            "control: a real read must extend the deadline"
        );
    }

    /// Concurrent counterpart of [`assert_peek_does_not_renew`].
    fn assert_peek_does_not_renew_concurrent<C>(store: C)
    where
        C: ConcurrentCached<u32, i32>
            + ConcurrentCacheExpiry<u32, i32>
            + ConcurrentCacheRefreshOnHit,
    {
        store.set_refresh_on_hit(true);
        let _ = store.cache_set(1, 100).unwrap();

        let (_, first) = store.cache_peek_expires_at(&1u32);
        std::thread::sleep(Duration::from_millis(40));
        let (_, second) = store.cache_peek_expires_at(&1u32);
        assert_eq!(
            first, second,
            "peeking must not renew the ttl even with refresh_on_hit enabled"
        );

        assert_eq!(store.cache_get(&1u32).unwrap(), Some(100));
        let (_, after_hit) = store.cache_peek_expires_at(&1u32);
        assert!(
            after_hit > first,
            "control: a real read must extend the deadline"
        );
    }

    #[test]
    fn refresh_on_hit_no_renewal_uniform_across_single_owner_ttl_stores() {
        assert_peek_does_not_renew(
            TtlCache::<u32, i32>::builder()
                .ttl(Duration::from_millis(250))
                .build()
                .unwrap(),
        );
        assert_peek_does_not_renew(
            LruTtlCache::<u32, i32>::builder()
                .max_size(8)
                .ttl(Duration::from_millis(250))
                .build()
                .unwrap(),
        );
    }

    #[test]
    fn refresh_on_hit_no_renewal_uniform_across_sharded_ttl_stores() {
        assert_peek_does_not_renew_concurrent(
            ShardedTtlCache::<u32, i32>::builder()
                .ttl(Duration::from_millis(250))
                .build()
                .unwrap(),
        );
        assert_peek_does_not_renew_concurrent(
            ShardedLruTtlCache::<u32, i32>::builder()
                .shards(1)
                .per_shard_max_size(8)
                .ttl(Duration::from_millis(250))
                .build()
                .unwrap(),
        );
    }

    /// The returned deadline is in the past exactly when `cache_peek_with_expiry_status` reports
    /// `true`. On these stores the deadline is a real clock reading (unlike the advisory
    /// `Expires`-based stores), so the two must never disagree.
    fn assert_deadline_matches_expiry_status<C>(mut store: C)
    where
        C: Cached<u32, i32> + CacheExpiry<u32, i32> + CloneCached<u32, i32>,
    {
        store.cache_set(1, 100);
        for _ in 0..2 {
            let (_, deadline) = store.cache_peek_expires_at(&1u32);
            let (_, expired) = store.cache_peek_with_expiry_status(&1u32);
            assert_eq!(
                deadline.is_some_and(|t| t <= Instant::now()),
                expired,
                "the deadline must be in the past exactly when the peek reports expired"
            );
            std::thread::sleep(PAST_TTL);
        }
    }

    /// Concurrent counterpart of [`assert_deadline_matches_expiry_status`].
    fn assert_deadline_matches_expiry_status_concurrent<C>(store: C)
    where
        C: ConcurrentCached<u32, i32>
            + ConcurrentCacheExpiry<u32, i32>
            + ConcurrentCloneCached<u32, i32>,
    {
        let _ = store.cache_set(1, 100).unwrap();
        for _ in 0..2 {
            let (_, deadline) = store.cache_peek_expires_at(&1u32);
            let (_, expired) = store.cache_peek_with_expiry_status(&1u32);
            assert_eq!(
                deadline.is_some_and(|t| t <= Instant::now()),
                expired,
                "the deadline must be in the past exactly when the peek reports expired"
            );
            std::thread::sleep(PAST_TTL);
        }
    }

    #[test]
    fn deadline_agrees_with_expiry_status_uniform_across_single_owner_ttl_stores() {
        assert_deadline_matches_expiry_status(
            TtlCache::<u32, i32>::builder()
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
        assert_deadline_matches_expiry_status(
            LruTtlCache::<u32, i32>::builder()
                .max_size(8)
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
        assert_deadline_matches_expiry_status(
            TtlSortedCache::<u32, i32>::builder()
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
    }

    #[test]
    fn deadline_agrees_with_expiry_status_uniform_across_sharded_ttl_stores() {
        assert_deadline_matches_expiry_status_concurrent(
            ShardedTtlCache::<u32, i32>::builder()
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
        assert_deadline_matches_expiry_status_concurrent(
            ShardedLruTtlCache::<u32, i32>::builder()
                .shards(1)
                .per_shard_max_size(8)
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
    }

    #[test]
    fn side_effect_free_uniform_across_single_owner_ttl_stores() {
        let mut ttl: TtlCache<u32, i32> = TtlCache::builder().ttl(LONG_TTL).build().unwrap();
        ttl.cache_set(1, 100);
        assert_side_effect_free_single_owner(&mut ttl, &1u32, &999u32);

        let mut lru_ttl: LruTtlCache<u32, i32> = LruTtlCache::builder()
            .max_size(8)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        lru_ttl.cache_set(1, 100);
        assert_side_effect_free_single_owner(&mut lru_ttl, &1u32, &999u32);

        let mut sorted: TtlSortedCache<u32, i32> =
            TtlSortedCache::builder().ttl(LONG_TTL).build().unwrap();
        sorted.cache_set(1, 100);
        assert_side_effect_free_single_owner(&mut sorted, &1u32, &999u32);
    }

    #[test]
    fn side_effect_free_uniform_across_sharded_ttl_stores() {
        let ttl: ShardedTtlCache<u32, i32> =
            ShardedTtlCache::builder().ttl(LONG_TTL).build().unwrap();
        let _ = ttl.cache_set(1, 100).unwrap();
        assert_side_effect_free_concurrent(&ttl, &1u32, &999u32);

        let lru_ttl: ShardedLruTtlCache<u32, i32> = ShardedLruTtlCache::builder()
            .shards(1)
            .per_shard_max_size(8)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        let _ = lru_ttl.cache_set(1, 100).unwrap();
        assert_side_effect_free_concurrent(&lru_ttl, &1u32, &999u32);
    }

    #[test]
    fn lru_no_promotion_uniform_across_single_owner_ttl_lru_store() {
        let store: LruTtlCache<u32, i32> = LruTtlCache::builder()
            .max_size(2)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        assert_peek_does_not_promote_lru_single_owner(store, 1u32, 100, 2u32, 200, 3u32, 300);
    }

    #[test]
    fn lru_no_promotion_uniform_across_sharded_ttl_lru_store() {
        let store: ShardedLruTtlCache<u32, i32> = ShardedLruTtlCache::builder()
            .shards(1)
            .per_shard_max_size(2)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        assert_peek_does_not_promote_lru_concurrent(store, 1u32, 100, 2u32, 200, 3u32, 300);
    }

    /// Certifies a real, pre-existing cross-family DIVERGENCE rather than uniformity, found
    /// independently while certifying the single-owner stores
    /// (`peek_expires_at_overflowing_ttl_reports_no_deadline` in `src/stores/ttl.rs`,
    /// `peek_expires_at_reports_no_deadline_under_ttl_overflow` in `src/stores/lru_ttl.rs` and
    /// `src/stores/ttl_sorted.rs`) and the sharded stores
    /// (`peek_expires_at_extreme_ttl_is_clamped_not_overflowed` in `src/stores/sharded/ttl.rs`
    /// and `src/stores/sharded/lru_ttl.rs`) in isolation.
    ///
    /// On every other input in this file, `cache_peek_expires_at` agrees across the whole TTL
    /// family. `Duration::MAX` is the one input where it does not: the single-owner stores keep
    /// the raw `Duration` and let `Instant::checked_add` overflow to `None` (`compute_expires_at`
    /// -- indistinguishable, through this API alone, from "TTL disabled"), while the sharded
    /// stores store the ttl as an atomic `u64` of nanoseconds and clamp it to `u64::MAX` nanos
    /// (~584 years) BEFORE computing the deadline, so `checked_add` never sees an overflowing
    /// input and a real, very-distant deadline comes back instead.
    ///
    /// This is a pre-existing, deliberate representation difference (`Duration` field vs. atomic
    /// nanosecond clamp), not a bug, and this test does not change either side. It exists so that
    /// a future change collapsing the two representations -- and therefore this asymmetry --
    /// fails loudly here, in the one file whose entire premise is cross-store uniformity, instead
    /// of only in the two isolated per-family unit tests above.
    #[test]
    fn extreme_ttl_diverges_between_single_owner_and_sharded_ttl_families() {
        // Single-owner family: Duration::MAX overflows Instant::checked_add, so the deadline
        // reports as (Some(v), None) -- the same shape as "TTL disabled".
        let mut ttl: TtlCache<u32, i32> = TtlCache::builder().ttl(SHORT_TTL).build().unwrap();
        ttl.set_ttl(Duration::MAX);
        ttl.cache_set(1, 100);
        assert_eq!(
            ttl.cache_peek_expires_at(&1u32),
            (Some(100), None),
            "TtlCache: an overflowing ttl must report no deadline, not a real one"
        );

        let mut lru_ttl: LruTtlCache<u32, i32> = LruTtlCache::builder()
            .max_size(8)
            .ttl(SHORT_TTL)
            .build()
            .unwrap();
        lru_ttl.set_ttl(Duration::MAX);
        lru_ttl.cache_set(1, 100);
        assert_eq!(
            lru_ttl.cache_peek_expires_at(&1u32),
            (Some(100), None),
            "LruTtlCache: an overflowing ttl must report no deadline, not a real one"
        );

        let mut sorted: TtlSortedCache<u32, i32> =
            TtlSortedCache::builder().ttl(SHORT_TTL).build().unwrap();
        sorted.set_ttl(Duration::MAX);
        sorted.cache_set(1, 100);
        assert_eq!(
            sorted.cache_peek_expires_at(&1u32),
            (Some(100), None),
            "TtlSortedCache: an overflowing ttl must report no deadline, not a real one"
        );

        // Sharded family: the ttl is clamped to u64::MAX nanoseconds before the deadline is
        // computed, so it never overflows -- a real, ~584-year-out deadline comes back instead of
        // None. A century out is a conservative, sleep-free lower bound well short of the actual
        // ~584-year clamp.
        let a_century = Duration::from_secs(60 * 60 * 24 * 365 * 100);

        let sharded_ttl: ShardedTtlCache<u32, i32> =
            ShardedTtlCache::builder().ttl(SHORT_TTL).build().unwrap();
        sharded_ttl.set_ttl(Duration::MAX);
        let _ = sharded_ttl.cache_set(1, 100).unwrap();
        let (value, deadline) = sharded_ttl.cache_peek_expires_at(&1u32);
        assert_eq!(value, Some(100));
        assert!(
            deadline.is_some_and(|t| t > Instant::now() + a_century),
            "ShardedTtlCache: an overflowing ttl must clamp to a real, very-distant deadline, \
             not None -- unlike TtlCache"
        );

        let sharded_lru_ttl: ShardedLruTtlCache<u32, i32> = ShardedLruTtlCache::builder()
            .shards(1)
            .per_shard_max_size(8)
            .ttl(SHORT_TTL)
            .build()
            .unwrap();
        sharded_lru_ttl.set_ttl(Duration::MAX);
        let _ = sharded_lru_ttl.cache_set(1, 100).unwrap();
        let (value, deadline) = sharded_lru_ttl.cache_peek_expires_at(&1u32);
        assert_eq!(value, Some(100));
        assert!(
            deadline.is_some_and(|t| t > Instant::now() + a_century),
            "ShardedLruTtlCache: an overflowing ttl must clamp to a real, very-distant deadline, \
             not None -- unlike LruTtlCache"
        );

        // The value-free read must show the identical split. This is the edge where the two
        // reads are most likely to drift apart, since each store computes the deadline on its
        // own path, so pin them against each other here rather than trusting the general
        // agreement property to have covered an overflowing ttl.
        assert_eq!(
            ttl.cache_expires_at(&1u32),
            (true, None),
            "TtlCache: the value-free read must report the same overflowed deadline as the peek"
        );
        assert_eq!(
            lru_ttl.cache_expires_at(&1u32),
            (true, None),
            "LruTtlCache: the value-free read must report the same overflowed deadline as the peek"
        );
        assert_eq!(
            sorted.cache_expires_at(&1u32),
            (true, None),
            "TtlSortedCache: the value-free read must report the same overflowed deadline as the \
             peek"
        );

        let (present, deadline) = sharded_ttl.cache_expires_at(&1u32);
        assert!(present, "ShardedTtlCache: the entry is present");
        assert!(
            deadline.is_some_and(|t| t > Instant::now() + a_century),
            "ShardedTtlCache: the value-free read must clamp like the peek, not report None"
        );

        let (present, deadline) = sharded_lru_ttl.cache_expires_at(&1u32);
        assert!(present, "ShardedLruTtlCache: the entry is present");
        assert!(
            deadline.is_some_and(|t| t > Instant::now() + a_century),
            "ShardedLruTtlCache: the value-free read must clamp like the peek, not report None"
        );
    }

    /// The TTL-family half of the borrowed-key property certified for the `Expires` stores in
    /// `expires_stores::peek_expires_at_accepts_a_borrowed_key_on_single_owner_expires_stores`.
    /// Here the deadline is a real clock reading, so the `&str` peek must report a live
    /// deadline, not just the same value.
    #[test]
    fn peek_expires_at_accepts_a_borrowed_key_on_single_owner_ttl_stores() {
        let mut ttl: TtlCache<String, i32> = TtlCache::builder().ttl(LONG_TTL).build().unwrap();
        ttl.cache_set("k".to_string(), 100);
        let (value, deadline) = peek_via_borrowed_str(&ttl, "k");
        assert_eq!(value, Some(100), "a &str peek must reach the &String entry");
        assert!(
            deadline.is_some_and(|t| t > Instant::now()),
            "the borrowed peek must report the live deadline, not None"
        );
        assert_eq!(
            ttl.peek_expires_at("k"),
            ttl.cache_peek_expires_at("k"),
            "the alias must accept the borrowed key too, and agree"
        );
        assert_eq!(
            peek_via_borrowed_str::<i32, _>(&ttl, "absent"),
            (None, None)
        );

        let mut lru_ttl: LruTtlCache<String, i32> = LruTtlCache::builder()
            .max_size(8)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        lru_ttl.cache_set("k".to_string(), 100);
        assert_eq!(peek_via_borrowed_str(&lru_ttl, "k").0, Some(100));

        let mut sorted: TtlSortedCache<String, i32> =
            TtlSortedCache::builder().ttl(LONG_TTL).build().unwrap();
        sorted.cache_set("k".to_string(), 100);
        assert_eq!(peek_via_borrowed_str(&sorted, "k").0, Some(100));
    }

    /// Combines the borrowed-key property with the LRU-no-promotion property
    /// (`lru_no_promotion_uniform_across_single_owner_ttl_lru_store` above): both are certified
    /// individually, but only ever with `Q == K`. A peek reached through `Q = str` must be just
    /// as side-effect-free on recency as one reached through `Q = String` -- a `Borrow`-specific
    /// codepath that renewed recency (e.g. one that fell through to a renewing lookup instead of
    /// the peek because the borrowed form was not wired the same way) would be invisible to
    /// every other test in this file.
    #[test]
    fn peek_via_borrowed_key_does_not_promote_lru_on_single_owner_ttl_lru_store() {
        let mut lru_ttl: LruTtlCache<String, i32> = LruTtlCache::builder()
            .max_size(2)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        lru_ttl.cache_set("k1".to_string(), 100); // LRU
        lru_ttl.cache_set("k2".to_string(), 200); // MRU

        // Peek the LRU key through a borrowed &str, repeatedly. Must not promote it.
        for _ in 0..3 {
            assert_eq!(peek_via_borrowed_str(&lru_ttl, "k1").0, Some(100));
        }

        // Overflow: k3 must evict the still-LRU key (k1), not k2.
        lru_ttl.cache_set("k3".to_string(), 300);

        assert_eq!(
            lru_ttl.cache_get(&"k1".to_string()),
            None,
            "a borrowed-key peek must not promote recency; k1 must still be the LRU victim"
        );
        assert_eq!(lru_ttl.cache_get(&"k2".to_string()), Some(&200));
        assert_eq!(lru_ttl.cache_get(&"k3".to_string()), Some(&300));
    }

    #[test]
    fn cache_expiry_reachable_via_prelude_and_generic_bound_ttl_stores() {
        let mut ttl: TtlCache<u32, i32> = TtlCache::builder().ttl(LONG_TTL).build().unwrap();
        ttl.cache_set(1, 100);
        assert_eq!(peek_via_generic_bound(&ttl, &1u32).0, Some(100));

        let mut lru_ttl: LruTtlCache<u32, i32> = LruTtlCache::builder()
            .max_size(8)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        lru_ttl.cache_set(1, 100);
        assert_eq!(peek_via_generic_bound(&lru_ttl, &1u32).0, Some(100));

        let mut sorted: TtlSortedCache<u32, i32> =
            TtlSortedCache::builder().ttl(LONG_TTL).build().unwrap();
        sorted.cache_set(1, 100);
        assert_eq!(peek_via_generic_bound(&sorted, &1u32).0, Some(100));
    }

    #[test]
    fn concurrent_cache_expiry_reachable_via_prelude_and_generic_bound_ttl_stores() {
        let ttl: ShardedTtlCache<u32, i32> =
            ShardedTtlCache::builder().ttl(LONG_TTL).build().unwrap();
        let _ = ttl.cache_set(1, 100).unwrap();
        assert_eq!(peek_via_generic_bound_concurrent(&ttl, &1u32).0, Some(100));

        let lru_ttl: ShardedLruTtlCache<u32, i32> = ShardedLruTtlCache::builder()
            .shards(1)
            .per_shard_max_size(8)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        let _ = lru_ttl.cache_set(1, 100).unwrap();
        assert_eq!(
            peek_via_generic_bound_concurrent(&lru_ttl, &1u32).0,
            Some(100)
        );
    }

    // ── The value-free read (`cache_expires_at`) on the TTL family ────────────────────────────

    /// The four return shapes of `cache_expires_at`, the value-free counterpart of
    /// [`assert_four_quadrants`]: `(false, None)` absent, `(true, None)` present with no deadline,
    /// `(true, Some(future))` live, `(true, Some(past))` expired and not removed.
    fn assert_four_shapes_expires_at<C>(mut store: C)
    where
        C: Cached<u32, i32> + CacheTtl + CacheExpiry<u32, i32>,
    {
        // (false, None): absent key.
        assert_eq!(store.cache_expires_at(&404u32), (false, None));

        // (true, None): present, no known deadline (ttl disabled at insert time). The presence
        // flag is what separates this from the absent case above; the deadline alone cannot.
        let original_ttl = store.ttl();
        store.unset_ttl();
        store.cache_set(1, 100);
        assert_eq!(store.cache_expires_at(&1u32), (true, None));
        if let Some(ttl) = original_ttl {
            store.set_ttl(ttl);
        }

        // (true, Some(future)): live.
        store.cache_set(2, 200);
        let (present, deadline) = store.cache_expires_at(&2u32);
        assert!(present);
        assert!(
            deadline.is_some_and(|t| t > Instant::now()),
            "a live entry's deadline must be in the future"
        );

        // (true, Some(past)): expired, not removed.
        store.cache_set(3, 300);
        std::thread::sleep(PAST_TTL);
        let (present, deadline) = store.cache_expires_at(&3u32);
        assert!(present);
        assert!(
            deadline.is_some_and(|t| t <= Instant::now()),
            "an expired entry's deadline must be in the past"
        );
        assert_eq!(
            store.cache_expires_at(&3u32),
            (true, deadline),
            "an expired entry must survive the read"
        );
        assert_eq!(
            store.expires_at(&3u32),
            (true, deadline),
            "the alias must agree with the required method"
        );
    }

    /// Concurrent counterpart of [`assert_four_shapes_expires_at`].
    fn assert_four_shapes_expires_at_concurrent<C>(store: C)
    where
        C: ConcurrentCached<u32, i32> + ConcurrentCacheTtl + ConcurrentCacheExpiry<u32, i32>,
    {
        assert_eq!(store.cache_expires_at(&404u32), (false, None));

        let original_ttl = store.ttl();
        store.unset_ttl();
        let _ = store.cache_set(1, 100).unwrap();
        assert_eq!(store.cache_expires_at(&1u32), (true, None));
        if let Some(ttl) = original_ttl {
            store.set_ttl(ttl);
        }

        let _ = store.cache_set(2, 200).unwrap();
        let (present, deadline) = store.cache_expires_at(&2u32);
        assert!(present);
        assert!(
            deadline.is_some_and(|t| t > Instant::now()),
            "a live entry's deadline must be in the future"
        );

        let _ = store.cache_set(3, 300).unwrap();
        std::thread::sleep(PAST_TTL);
        let (present, deadline) = store.cache_expires_at(&3u32);
        assert!(present);
        assert!(
            deadline.is_some_and(|t| t <= Instant::now()),
            "an expired entry's deadline must be in the past"
        );
        assert_eq!(
            store.cache_expires_at(&3u32),
            (true, deadline),
            "an expired entry must survive the read"
        );
        assert_eq!(
            store.expires_at(&3u32),
            (true, deadline),
            "the alias must agree with the required method"
        );
    }

    #[test]
    fn expires_at_four_shapes_uniform_across_single_owner_ttl_stores() {
        assert_four_shapes_expires_at(
            TtlCache::<u32, i32>::builder()
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
        assert_four_shapes_expires_at(
            LruTtlCache::<u32, i32>::builder()
                .max_size(8)
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
        assert_four_shapes_expires_at(
            TtlSortedCache::<u32, i32>::builder()
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
    }

    #[test]
    fn expires_at_four_shapes_uniform_across_sharded_ttl_stores() {
        assert_four_shapes_expires_at_concurrent(
            ShardedTtlCache::<u32, i32>::builder()
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
        assert_four_shapes_expires_at_concurrent(
            ShardedLruTtlCache::<u32, i32>::builder()
                .shards(1)
                .per_shard_max_size(8)
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
    }

    /// Drives [`assert_reads_agree_single_owner`] over a key in every quadrant: absent, present
    /// with no deadline, live, and expired. The setup is asserted to have actually produced each
    /// quadrant, so agreement cannot pass vacuously (e.g. by every key being absent).
    fn assert_reads_agree_ttl<C>(mut store: C)
    where
        C: Cached<u32, i32> + CacheTtl + CacheExpiry<u32, i32>,
    {
        // Absent.
        assert_eq!(store.cache_expires_at(&404u32), (false, None));
        assert_reads_agree_single_owner(&store, &404u32);

        // Present, no deadline.
        let original_ttl = store.ttl();
        store.unset_ttl();
        store.cache_set(1, 100);
        if let Some(ttl) = original_ttl {
            store.set_ttl(ttl);
        }
        assert_eq!(
            store.cache_expires_at(&1u32),
            (true, None),
            "setup: entry 1 must be present with no deadline"
        );
        assert_reads_agree_single_owner(&store, &1u32);

        // Live, then the same entry expired. Nothing is inserted after the sleep, so a store that
        // sweeps on write cannot quietly turn the expired quadrant into the absent one.
        store.cache_set(2, 200);
        assert!(
            store
                .cache_expires_at(&2u32)
                .1
                .is_some_and(|t| t > Instant::now()),
            "setup: entry 2 must be live"
        );
        assert_reads_agree_single_owner(&store, &2u32);

        std::thread::sleep(PAST_TTL);
        assert!(
            store
                .cache_expires_at(&2u32)
                .1
                .is_some_and(|t| t <= Instant::now()),
            "setup: entry 2 must now be expired"
        );
        assert_reads_agree_single_owner(&store, &2u32);
    }

    /// Concurrent counterpart of [`assert_reads_agree_ttl`].
    fn assert_reads_agree_ttl_concurrent<C>(store: C)
    where
        C: ConcurrentCached<u32, i32> + ConcurrentCacheTtl + ConcurrentCacheExpiry<u32, i32>,
    {
        assert_eq!(store.cache_expires_at(&404u32), (false, None));
        assert_reads_agree_concurrent(&store, &404u32);

        let original_ttl = store.ttl();
        store.unset_ttl();
        let _ = store.cache_set(1, 100).unwrap();
        if let Some(ttl) = original_ttl {
            store.set_ttl(ttl);
        }
        assert_eq!(
            store.cache_expires_at(&1u32),
            (true, None),
            "setup: entry 1 must be present with no deadline"
        );
        assert_reads_agree_concurrent(&store, &1u32);

        let _ = store.cache_set(2, 200).unwrap();
        assert!(
            store
                .cache_expires_at(&2u32)
                .1
                .is_some_and(|t| t > Instant::now()),
            "setup: entry 2 must be live"
        );
        assert_reads_agree_concurrent(&store, &2u32);

        std::thread::sleep(PAST_TTL);
        assert!(
            store
                .cache_expires_at(&2u32)
                .1
                .is_some_and(|t| t <= Instant::now()),
            "setup: entry 2 must now be expired"
        );
        assert_reads_agree_concurrent(&store, &2u32);
    }

    #[test]
    fn the_two_reads_agree_uniform_across_single_owner_ttl_stores() {
        assert_reads_agree_ttl(
            TtlCache::<u32, i32>::builder()
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
        assert_reads_agree_ttl(
            LruTtlCache::<u32, i32>::builder()
                .max_size(8)
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
        assert_reads_agree_ttl(
            TtlSortedCache::<u32, i32>::builder()
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
    }

    #[test]
    fn the_two_reads_agree_uniform_across_sharded_ttl_stores() {
        assert_reads_agree_ttl_concurrent(
            ShardedTtlCache::<u32, i32>::builder()
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
        assert_reads_agree_ttl_concurrent(
            ShardedLruTtlCache::<u32, i32>::builder()
                .shards(1)
                .per_shard_max_size(8)
                .ttl(SHORT_TTL)
                .build()
                .unwrap(),
        );
    }

    #[test]
    fn expires_at_side_effect_free_uniform_across_single_owner_ttl_stores() {
        let mut ttl: TtlCache<u32, i32> = TtlCache::builder().ttl(LONG_TTL).build().unwrap();
        ttl.cache_set(1, 100);
        assert_expires_at_side_effect_free_single_owner(&mut ttl, &1u32, &999u32);

        let mut lru_ttl: LruTtlCache<u32, i32> = LruTtlCache::builder()
            .max_size(8)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        lru_ttl.cache_set(1, 100);
        assert_expires_at_side_effect_free_single_owner(&mut lru_ttl, &1u32, &999u32);

        let mut sorted: TtlSortedCache<u32, i32> =
            TtlSortedCache::builder().ttl(LONG_TTL).build().unwrap();
        sorted.cache_set(1, 100);
        assert_expires_at_side_effect_free_single_owner(&mut sorted, &1u32, &999u32);
    }

    #[test]
    fn expires_at_side_effect_free_uniform_across_sharded_ttl_stores() {
        let ttl: ShardedTtlCache<u32, i32> =
            ShardedTtlCache::builder().ttl(LONG_TTL).build().unwrap();
        let _ = ttl.cache_set(1, 100).unwrap();
        assert_expires_at_side_effect_free_concurrent(&ttl, &1u32, &999u32);

        let lru_ttl: ShardedLruTtlCache<u32, i32> = ShardedLruTtlCache::builder()
            .shards(1)
            .per_shard_max_size(8)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        let _ = lru_ttl.cache_set(1, 100).unwrap();
        assert_expires_at_side_effect_free_concurrent(&lru_ttl, &1u32, &999u32);
    }

    #[test]
    fn expires_at_lru_no_promotion_uniform_across_single_owner_ttl_lru_store() {
        let store: LruTtlCache<u32, i32> = LruTtlCache::builder()
            .max_size(2)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        assert_expires_at_does_not_promote_lru_single_owner(store, 1u32, 100, 2u32, 200, 3u32, 300);
    }

    #[test]
    fn expires_at_lru_no_promotion_uniform_across_sharded_ttl_lru_store() {
        let store: ShardedLruTtlCache<u32, i32> = ShardedLruTtlCache::builder()
            .shards(1)
            .per_shard_max_size(2)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        assert_expires_at_does_not_promote_lru_concurrent(store, 1u32, 100, 2u32, 200, 3u32, 300);
    }

    /// No deadline movement across two value-free reads with `set_refresh_on_hit(true)`, the
    /// counterpart of [`assert_peek_does_not_renew`]. The trait docs list "no TTL renewal" as part
    /// of the same no-side-effect contract, and no counter checked by
    /// [`assert_expires_at_side_effect_free_single_owner`] would surface a renewal.
    fn assert_expires_at_does_not_renew<C>(mut store: C)
    where
        C: Cached<u32, i32> + CacheExpiry<u32, i32> + CacheRefreshOnHit,
    {
        store.set_refresh_on_hit(true);
        store.cache_set(1, 100);

        let (_, first) = store.cache_expires_at(&1u32);
        std::thread::sleep(Duration::from_millis(40));
        let (_, second) = store.cache_expires_at(&1u32);
        assert_eq!(
            first, second,
            "a value-free read must not renew the ttl even with refresh_on_hit enabled"
        );

        // Control: a real read DOES renew, proving the assertion above is not vacuous.
        assert_eq!(store.cache_get(&1u32), Some(&100));
        let (_, after_hit) = store.cache_expires_at(&1u32);
        assert!(
            after_hit > first,
            "control: a real read must extend the deadline"
        );
    }

    /// Concurrent counterpart of [`assert_expires_at_does_not_renew`].
    fn assert_expires_at_does_not_renew_concurrent<C>(store: C)
    where
        C: ConcurrentCached<u32, i32>
            + ConcurrentCacheExpiry<u32, i32>
            + ConcurrentCacheRefreshOnHit,
    {
        store.set_refresh_on_hit(true);
        let _ = store.cache_set(1, 100).unwrap();

        let (_, first) = store.cache_expires_at(&1u32);
        std::thread::sleep(Duration::from_millis(40));
        let (_, second) = store.cache_expires_at(&1u32);
        assert_eq!(
            first, second,
            "a value-free read must not renew the ttl even with refresh_on_hit enabled"
        );

        assert_eq!(store.cache_get(&1u32).unwrap(), Some(100));
        let (_, after_hit) = store.cache_expires_at(&1u32);
        assert!(
            after_hit > first,
            "control: a real read must extend the deadline"
        );
    }

    #[test]
    fn expires_at_refresh_on_hit_no_renewal_uniform_across_single_owner_ttl_stores() {
        assert_expires_at_does_not_renew(
            TtlCache::<u32, i32>::builder()
                .ttl(Duration::from_millis(250))
                .build()
                .unwrap(),
        );
        assert_expires_at_does_not_renew(
            LruTtlCache::<u32, i32>::builder()
                .max_size(8)
                .ttl(Duration::from_millis(250))
                .build()
                .unwrap(),
        );
    }

    #[test]
    fn expires_at_refresh_on_hit_no_renewal_uniform_across_sharded_ttl_stores() {
        assert_expires_at_does_not_renew_concurrent(
            ShardedTtlCache::<u32, i32>::builder()
                .ttl(Duration::from_millis(250))
                .build()
                .unwrap(),
        );
        assert_expires_at_does_not_renew_concurrent(
            ShardedLruTtlCache::<u32, i32>::builder()
                .shards(1)
                .per_shard_max_size(8)
                .ttl(Duration::from_millis(250))
                .build()
                .unwrap(),
        );
    }

    /// The value-free counterpart of
    /// [`peek_expires_at_accepts_a_borrowed_key_on_single_owner_ttl_stores`]: `Q = str` with
    /// `Q != K`, reported deadline live rather than merely present.
    #[test]
    fn expires_at_accepts_a_borrowed_key_on_single_owner_ttl_stores() {
        let mut ttl: TtlCache<String, i32> = TtlCache::builder().ttl(LONG_TTL).build().unwrap();
        ttl.cache_set("k".to_string(), 100);
        let (present, deadline) = expires_at_via_borrowed_str(&ttl, "k");
        assert!(present, "a &str read must reach the &String entry");
        assert!(
            deadline.is_some_and(|t| t > Instant::now()),
            "the borrowed read must report the live deadline, not None"
        );
        assert_eq!(
            expires_at_via_borrowed_str(&ttl, "k"),
            ttl.cache_expires_at(&"k".to_string()),
            "the borrowed and owned key forms must not diverge"
        );
        assert_eq!(
            ttl.expires_at("k"),
            ttl.cache_expires_at("k"),
            "the alias must accept the borrowed key too, and agree"
        );
        assert_eq!(
            expires_at_via_borrowed_str::<i32, _>(&ttl, "absent"),
            (false, None)
        );

        let mut lru_ttl: LruTtlCache<String, i32> = LruTtlCache::builder()
            .max_size(8)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        lru_ttl.cache_set("k".to_string(), 100);
        assert!(expires_at_via_borrowed_str(&lru_ttl, "k").0);

        let mut sorted: TtlSortedCache<String, i32> =
            TtlSortedCache::builder().ttl(LONG_TTL).build().unwrap();
        sorted.cache_set("k".to_string(), 100);
        assert!(expires_at_via_borrowed_str(&sorted, "k").0);
    }

    /// The borrowed-key form combined with LRU-no-promotion, matching
    /// [`peek_via_borrowed_key_does_not_promote_lru_on_single_owner_ttl_lru_store`]. Both are
    /// certified individually, but only ever with `Q == K` for the value-free read.
    #[test]
    fn expires_at_via_borrowed_key_does_not_promote_lru_on_single_owner_ttl_lru_store() {
        let mut lru_ttl: LruTtlCache<String, i32> = LruTtlCache::builder()
            .max_size(2)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        lru_ttl.cache_set("k1".to_string(), 100); // LRU
        lru_ttl.cache_set("k2".to_string(), 200); // MRU

        for _ in 0..3 {
            assert!(expires_at_via_borrowed_str(&lru_ttl, "k1").0);
        }

        lru_ttl.cache_set("k3".to_string(), 300);

        assert_eq!(
            lru_ttl.cache_get(&"k1".to_string()),
            None,
            "a borrowed-key deadline read must not promote recency; k1 must still be the victim"
        );
        assert_eq!(lru_ttl.cache_get(&"k2".to_string()), Some(&200));
        assert_eq!(lru_ttl.cache_get(&"k3".to_string()), Some(&300));
    }

    #[test]
    fn cache_expires_at_reachable_via_prelude_and_generic_bound_ttl_stores() {
        let mut ttl: TtlCache<u32, i32> = TtlCache::builder().ttl(LONG_TTL).build().unwrap();
        ttl.cache_set(1, 100);
        assert!(expires_at_via_generic_bound(&ttl, &1u32).0);

        let mut lru_ttl: LruTtlCache<u32, i32> = LruTtlCache::builder()
            .max_size(8)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        lru_ttl.cache_set(1, 100);
        assert!(expires_at_via_generic_bound(&lru_ttl, &1u32).0);

        let mut sorted: TtlSortedCache<u32, i32> =
            TtlSortedCache::builder().ttl(LONG_TTL).build().unwrap();
        sorted.cache_set(1, 100);
        assert!(expires_at_via_generic_bound(&sorted, &1u32).0);
    }

    #[test]
    fn concurrent_cache_expires_at_reachable_via_prelude_and_generic_bound_ttl_stores() {
        let ttl: ShardedTtlCache<u32, i32> =
            ShardedTtlCache::builder().ttl(LONG_TTL).build().unwrap();
        let _ = ttl.cache_set(1, 100).unwrap();
        assert!(expires_at_via_generic_bound_concurrent(&ttl, &1u32).0);

        let lru_ttl: ShardedLruTtlCache<u32, i32> = ShardedLruTtlCache::builder()
            .shards(1)
            .per_shard_max_size(8)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        let _ = lru_ttl.cache_set(1, 100).unwrap();
        assert!(expires_at_via_generic_bound_concurrent(&lru_ttl, &1u32).0);
    }

    /// Property 8 on the TTL family: a real deadline is readable for a value type that does not
    /// implement `Clone`. `cache_peek_expires_at` cannot be called at all here -- its `V: Clone`
    /// bound is unsatisfiable -- so this is exactly the gap `cache_expires_at` was added to close.
    ///
    /// The sharded stores are reached through their absent-key path only: their own
    /// `ConcurrentCached` impls require `V: Clone` to insert, so a non-`Clone` entry cannot be put
    /// there through the public API. Constructing the store at `V = NotClone` and calling the read
    /// still pins that `ConcurrentCacheExpiry` itself puts no `Clone` bound on `V`.
    #[test]
    fn expires_at_reads_a_deadline_for_a_non_clone_value_on_ttl_stores() {
        let mut ttl: TtlCache<u32, NotClone> = TtlCache::builder().ttl(LONG_TTL).build().unwrap();
        ttl.cache_set(1, NotClone(7));
        assert_eq!(ttl.cache_get(&1u32), Some(&NotClone(7)));
        let (present, deadline) = expires_at_via_generic_bound(&ttl, &1u32);
        assert!(present);
        assert!(
            deadline.is_some_and(|t| t > Instant::now()),
            "the deadline must be readable without cloning the value"
        );
        assert_eq!(expires_at_via_generic_bound(&ttl, &999u32), (false, None));

        let mut lru_ttl: LruTtlCache<u32, NotClone> = LruTtlCache::builder()
            .max_size(8)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        lru_ttl.cache_set(1, NotClone(7));
        assert!(
            expires_at_via_generic_bound(&lru_ttl, &1u32)
                .1
                .is_some_and(|t| t > Instant::now())
        );

        let mut sorted: TtlSortedCache<u32, NotClone> =
            TtlSortedCache::builder().ttl(LONG_TTL).build().unwrap();
        sorted.cache_set(1, NotClone(7));
        assert!(
            expires_at_via_generic_bound(&sorted, &1u32)
                .1
                .is_some_and(|t| t > Instant::now())
        );

        let sharded: ShardedTtlCache<u32, NotClone> =
            ShardedTtlCache::builder().ttl(LONG_TTL).build().unwrap();
        assert_eq!(
            expires_at_via_generic_bound_concurrent(&sharded, &1u32),
            (false, None)
        );

        let sharded_lru: ShardedLruTtlCache<u32, NotClone> = ShardedLruTtlCache::builder()
            .shards(1)
            .per_shard_max_size(8)
            .ttl(LONG_TTL)
            .build()
            .unwrap();
        assert_eq!(
            expires_at_via_generic_bound_concurrent(&sharded_lru, &1u32),
            (false, None)
        );
    }
}

// ── `Expires`-based stores: ExpiringCache, ExpiringLruCache, ShardedExpiringCache, ────────────
// ── ShardedExpiringLruCache. Advisory deadlines; ungated. ─────────────────────────────────────

mod expires_stores {
    use super::*;
    use cached::{ExpiringCache, ExpiringLruCache, ShardedExpiringCache, ShardedExpiringLruCache};

    /// The four-quadrant `CacheExpiry` contract PLUS the advisory-caveat property (#4), generic
    /// over any single-owner `Expires`-based store. Invoked with the identical body against both
    /// single-owner `Expires` stores below.
    fn assert_four_quadrants_and_advisory_caveat<C>(mut store: C)
    where
        C: Cached<u32, AdvisoryToken>
            + CacheExpiry<u32, AdvisoryToken>
            + CloneCached<u32, AdvisoryToken>,
    {
        // (None, None): absent key.
        assert_eq!(store.cache_peek_expires_at(&404u32), (None, None));

        // (Some(v), None): present, no known deadline -- the value does not override `expires_at`.
        store.cache_set(1, AdvisoryToken::live(100));
        assert_eq!(
            store.cache_peek_expires_at(&1u32),
            (Some(AdvisoryToken::live(100)), None)
        );

        // (Some(v), Some(future)): live, with an overridden future deadline.
        let future = Instant::now() + Duration::from_secs(60);
        store.cache_set(2, AdvisoryToken::live_with_deadline(200, future));
        assert_eq!(
            store.cache_peek_expires_at(&2u32),
            (
                Some(AdvisoryToken::live_with_deadline(200, future)),
                Some(future)
            )
        );

        // (Some(v), Some(past)): present but expired, with an overridden past deadline; not
        // removed by the peek.
        let past = Instant::now() - Duration::from_millis(50);
        store.cache_set(3, AdvisoryToken::stale_with_deadline(300, past));
        assert_eq!(
            store.cache_peek_expires_at(&3u32),
            (
                Some(AdvisoryToken::stale_with_deadline(300, past)),
                Some(past)
            )
        );
        assert_eq!(
            store.cache_peek_expires_at(&3u32),
            (
                Some(AdvisoryToken::stale_with_deadline(300, past)),
                Some(past)
            ),
            "an expired entry must survive the peek"
        );

        // The advisory caveat (#4): a value that IS expired (`is_expired() == true`) but does not
        // override `expires_at` must report `None`, not a false liveness signal. `is_expired`
        // (via `cache_peek_with_expiry_status`) remains the authority.
        store.cache_set(4, AdvisoryToken::stale_no_deadline(400));
        assert_eq!(
            store.cache_peek_expires_at(&4u32),
            (Some(AdvisoryToken::stale_no_deadline(400)), None),
            "an expired value with no expires_at override must report None, not proof of liveness"
        );
        assert_eq!(
            store.cache_peek_with_expiry_status(&4u32),
            (Some(AdvisoryToken::stale_no_deadline(400)), true),
            "is_expired must remain the liveness authority even though the deadline read is None"
        );
    }

    /// Concurrent counterpart of [`assert_four_quadrants_and_advisory_caveat`].
    fn assert_four_quadrants_and_advisory_caveat_concurrent<C>(store: C)
    where
        C: ConcurrentCached<u32, AdvisoryToken>
            + ConcurrentCacheExpiry<u32, AdvisoryToken>
            + ConcurrentCloneCached<u32, AdvisoryToken>,
    {
        assert_eq!(store.cache_peek_expires_at(&404u32), (None, None));

        let _ = store.cache_set(1, AdvisoryToken::live(100)).unwrap();
        assert_eq!(
            store.cache_peek_expires_at(&1u32),
            (Some(AdvisoryToken::live(100)), None)
        );

        let future = Instant::now() + Duration::from_secs(60);
        let _ = store
            .cache_set(2, AdvisoryToken::live_with_deadline(200, future))
            .unwrap();
        assert_eq!(
            store.cache_peek_expires_at(&2u32),
            (
                Some(AdvisoryToken::live_with_deadline(200, future)),
                Some(future)
            )
        );

        let past = Instant::now() - Duration::from_millis(50);
        let _ = store
            .cache_set(3, AdvisoryToken::stale_with_deadline(300, past))
            .unwrap();
        assert_eq!(
            store.cache_peek_expires_at(&3u32),
            (
                Some(AdvisoryToken::stale_with_deadline(300, past)),
                Some(past)
            )
        );
        assert_eq!(
            store.cache_peek_expires_at(&3u32),
            (
                Some(AdvisoryToken::stale_with_deadline(300, past)),
                Some(past)
            ),
            "an expired entry must survive the peek"
        );

        let _ = store
            .cache_set(4, AdvisoryToken::stale_no_deadline(400))
            .unwrap();
        assert_eq!(
            store.cache_peek_expires_at(&4u32),
            (Some(AdvisoryToken::stale_no_deadline(400)), None),
            "an expired value with no expires_at override must report None, not proof of liveness"
        );
        assert_eq!(
            store.cache_peek_with_expiry_status(&4u32),
            (Some(AdvisoryToken::stale_no_deadline(400)), true),
            "is_expired must remain the liveness authority even though the deadline read is None"
        );
    }

    #[test]
    fn four_quadrants_and_advisory_caveat_uniform_across_single_owner_expires_stores() {
        assert_four_quadrants_and_advisory_caveat(
            ExpiringCache::<u32, AdvisoryToken>::builder()
                .build()
                .unwrap(),
        );
        assert_four_quadrants_and_advisory_caveat(
            ExpiringLruCache::<u32, AdvisoryToken>::builder()
                .max_size(8)
                .build()
                .unwrap(),
        );
    }

    #[test]
    fn four_quadrants_and_advisory_caveat_uniform_across_sharded_expires_stores() {
        assert_four_quadrants_and_advisory_caveat_concurrent(
            ShardedExpiringCache::<u32, AdvisoryToken>::builder()
                .build()
                .unwrap(),
        );
        assert_four_quadrants_and_advisory_caveat_concurrent(
            ShardedExpiringLruCache::<u32, AdvisoryToken>::builder()
                .shards(1)
                .per_shard_max_size(8)
                .build()
                .unwrap(),
        );
    }

    #[test]
    fn side_effect_free_uniform_across_single_owner_expires_stores() {
        let mut plain: ExpiringCache<u32, AdvisoryToken> =
            ExpiringCache::builder().build().unwrap();
        plain.cache_set(1, AdvisoryToken::live(100));
        assert_side_effect_free_single_owner(&mut plain, &1u32, &999u32);

        let mut lru: ExpiringLruCache<u32, AdvisoryToken> =
            ExpiringLruCache::builder().max_size(8).build().unwrap();
        lru.cache_set(1, AdvisoryToken::live(100));
        assert_side_effect_free_single_owner(&mut lru, &1u32, &999u32);
    }

    #[test]
    fn side_effect_free_uniform_across_sharded_expires_stores() {
        let plain: ShardedExpiringCache<u32, AdvisoryToken> =
            ShardedExpiringCache::builder().build().unwrap();
        let _ = plain.cache_set(1, AdvisoryToken::live(100)).unwrap();
        assert_side_effect_free_concurrent(&plain, &1u32, &999u32);

        let lru: ShardedExpiringLruCache<u32, AdvisoryToken> = ShardedExpiringLruCache::builder()
            .shards(1)
            .per_shard_max_size(8)
            .build()
            .unwrap();
        let _ = lru.cache_set(1, AdvisoryToken::live(100)).unwrap();
        assert_side_effect_free_concurrent(&lru, &1u32, &999u32);
    }

    #[test]
    fn lru_no_promotion_uniform_across_single_owner_expires_lru_store() {
        let store: ExpiringLruCache<u32, AdvisoryToken> =
            ExpiringLruCache::builder().max_size(2).build().unwrap();
        assert_peek_does_not_promote_lru_single_owner(
            store,
            1u32,
            AdvisoryToken::live(100),
            2u32,
            AdvisoryToken::live(200),
            3u32,
            AdvisoryToken::live(300),
        );
    }

    #[test]
    fn lru_no_promotion_uniform_across_sharded_expires_lru_store() {
        let store: ShardedExpiringLruCache<u32, AdvisoryToken> = ShardedExpiringLruCache::builder()
            .shards(1)
            .per_shard_max_size(2)
            .build()
            .unwrap();
        assert_peek_does_not_promote_lru_concurrent(
            store,
            1u32,
            AdvisoryToken::live(100),
            2u32,
            AdvisoryToken::live(200),
            3u32,
            AdvisoryToken::live(300),
        );
    }

    /// Property 6 (the `K: Borrow<Q>` generality): a `String`-keyed store peeked with a
    /// `&str`, i.e. `Q = str`, so `Q != K`. Every other call site in this file and in the
    /// per-store unit tests passes `Q == K`, which leaves the one bound that distinguishes
    /// `CacheExpiry` from `ConcurrentCacheExpiry` unexercised. The borrowed peek must reach
    /// the same entry as the owned one, through both the required method and the alias, and
    /// must report an absent key as `(None, None)` like any other miss.
    ///
    /// Run here (in the ungated module) rather than only under `time_stores`, so the
    /// borrowed-key path is certified even with zero features enabled.
    #[test]
    fn peek_expires_at_accepts_a_borrowed_key_on_single_owner_expires_stores() {
        let deadline = Instant::now() + Duration::from_secs(60);
        let stored = AdvisoryToken::live_with_deadline(1, deadline);

        let mut plain: ExpiringCache<String, AdvisoryToken> =
            ExpiringCache::builder().build().unwrap();
        plain.cache_set("k".to_string(), stored.clone());
        assert_eq!(
            peek_via_borrowed_str(&plain, "k"),
            (Some(stored.clone()), Some(deadline)),
            "a &str peek must reach the same entry a &String peek does"
        );
        assert_eq!(
            peek_via_borrowed_str(&plain, "k"),
            plain.cache_peek_expires_at(&"k".to_string()),
            "the borrowed and owned key forms must not diverge"
        );
        assert_eq!(
            plain.peek_expires_at("k"),
            plain.cache_peek_expires_at("k"),
            "the alias must accept the borrowed key too, and agree"
        );
        assert_eq!(
            peek_via_borrowed_str::<AdvisoryToken, _>(&plain, "absent"),
            (None, None)
        );

        let mut lru: ExpiringLruCache<String, AdvisoryToken> =
            ExpiringLruCache::builder().max_size(8).build().unwrap();
        lru.cache_set("k".to_string(), stored.clone());
        assert_eq!(
            peek_via_borrowed_str(&lru, "k"),
            (Some(stored), Some(deadline))
        );
        assert_eq!(
            peek_via_borrowed_str::<AdvisoryToken, _>(&lru, "absent"),
            (None, None)
        );
    }

    /// A second `Borrow<Q>` shape, distinct from `String`/`str`: `K = Vec<u8>` peeked through
    /// `Q = [u8]`. See [`peek_via_borrowed_slice`] for why `String`/`str` alone is not enough to
    /// certify the bound is fully generic.
    #[test]
    fn peek_expires_at_accepts_a_borrowed_slice_key_on_single_owner_expires_stores() {
        let deadline = Instant::now() + Duration::from_secs(60);
        let stored = AdvisoryToken::live_with_deadline(7, deadline);

        let mut plain: ExpiringCache<Vec<u8>, AdvisoryToken> =
            ExpiringCache::builder().build().unwrap();
        plain.cache_set(vec![1, 2, 3], stored.clone());
        assert_eq!(
            peek_via_borrowed_slice(&plain, &[1, 2, 3]),
            (Some(stored.clone()), Some(deadline)),
            "a &[u8] peek must reach the same entry a &Vec<u8> peek does"
        );
        assert_eq!(
            peek_via_borrowed_slice(&plain, &[1, 2, 3]),
            plain.cache_peek_expires_at(&vec![1, 2, 3]),
            "the borrowed and owned key forms must not diverge"
        );
        assert_eq!(
            peek_via_borrowed_slice::<AdvisoryToken, _>(&plain, &[9, 9, 9]),
            (None, None)
        );
    }

    /// Combines the borrowed-key property with the LRU-no-promotion property
    /// (`lru_no_promotion_uniform_across_single_owner_expires_lru_store` above): both are
    /// certified individually, but only ever with `Q == K`. See the TTL-family counterpart
    /// `ttl_stores::peek_via_borrowed_key_does_not_promote_lru_on_single_owner_ttl_lru_store`
    /// for why the combination needs its own test.
    #[test]
    fn peek_via_borrowed_key_does_not_promote_lru_on_single_owner_expires_lru_store() {
        let mut lru: ExpiringLruCache<String, AdvisoryToken> =
            ExpiringLruCache::builder().max_size(2).build().unwrap();
        lru.cache_set("k1".to_string(), AdvisoryToken::live(100)); // LRU
        lru.cache_set("k2".to_string(), AdvisoryToken::live(200)); // MRU

        for _ in 0..3 {
            assert_eq!(
                peek_via_borrowed_str(&lru, "k1").0,
                Some(AdvisoryToken::live(100))
            );
        }

        lru.cache_set("k3".to_string(), AdvisoryToken::live(300));

        assert_eq!(
            lru.cache_get(&"k1".to_string()),
            None,
            "a borrowed-key peek must not promote recency; k1 must still be the LRU victim"
        );
        assert_eq!(
            lru.cache_get(&"k2".to_string()),
            Some(&AdvisoryToken::live(200))
        );
        assert_eq!(
            lru.cache_get(&"k3".to_string()),
            Some(&AdvisoryToken::live(300))
        );
    }

    #[test]
    fn cache_expiry_reachable_via_prelude_and_generic_bound_expires_stores() {
        let mut plain: ExpiringCache<u32, AdvisoryToken> =
            ExpiringCache::builder().build().unwrap();
        plain.cache_set(1, AdvisoryToken::live(100));
        assert_eq!(
            peek_via_generic_bound(&plain, &1u32).0,
            Some(AdvisoryToken::live(100))
        );

        let mut lru: ExpiringLruCache<u32, AdvisoryToken> =
            ExpiringLruCache::builder().max_size(8).build().unwrap();
        lru.cache_set(1, AdvisoryToken::live(100));
        assert_eq!(
            peek_via_generic_bound(&lru, &1u32).0,
            Some(AdvisoryToken::live(100))
        );
    }

    #[test]
    fn concurrent_cache_expiry_reachable_via_prelude_and_generic_bound_expires_stores() {
        let plain: ShardedExpiringCache<u32, AdvisoryToken> =
            ShardedExpiringCache::builder().build().unwrap();
        let _ = plain.cache_set(1, AdvisoryToken::live(100)).unwrap();
        assert_eq!(
            peek_via_generic_bound_concurrent(&plain, &1u32).0,
            Some(AdvisoryToken::live(100))
        );

        let lru: ShardedExpiringLruCache<u32, AdvisoryToken> = ShardedExpiringLruCache::builder()
            .shards(1)
            .per_shard_max_size(8)
            .build()
            .unwrap();
        let _ = lru.cache_set(1, AdvisoryToken::live(100)).unwrap();
        assert_eq!(
            peek_via_generic_bound_concurrent(&lru, &1u32).0,
            Some(AdvisoryToken::live(100))
        );
    }

    // ── The value-free read (`cache_expires_at`) on the `Expires` family ──────────────────────

    /// The four return shapes of `cache_expires_at` PLUS the advisory caveat, the value-free
    /// counterpart of [`assert_four_quadrants_and_advisory_caveat`].
    ///
    /// The caveat matters more here than on the peek: `(true, None)` on an entry that
    /// `is_expired` reports expired pins that the presence flag is independent of expiry, and
    /// that `None` is not evidence of liveness. A store that returned `(false, None)` for an
    /// expired entry (reporting it absent, or sweeping it) would fail here.
    fn assert_four_shapes_and_advisory_caveat_expires_at<C>(mut store: C)
    where
        C: Cached<u32, AdvisoryToken>
            + CacheExpiry<u32, AdvisoryToken>
            + CloneCached<u32, AdvisoryToken>,
    {
        // (false, None): absent key.
        assert_eq!(store.cache_expires_at(&404u32), (false, None));

        // (true, None): present, no known deadline -- the value does not override `expires_at`.
        store.cache_set(1, AdvisoryToken::live(100));
        assert_eq!(store.cache_expires_at(&1u32), (true, None));

        // (true, Some(future)): live, with an overridden future deadline.
        let future = Instant::now() + Duration::from_secs(60);
        store.cache_set(2, AdvisoryToken::live_with_deadline(200, future));
        assert_eq!(store.cache_expires_at(&2u32), (true, Some(future)));

        // (true, Some(past)): present but expired, with an overridden past deadline; not removed.
        let past = Instant::now() - Duration::from_millis(50);
        store.cache_set(3, AdvisoryToken::stale_with_deadline(300, past));
        assert_eq!(store.cache_expires_at(&3u32), (true, Some(past)));
        assert_eq!(
            store.cache_expires_at(&3u32),
            (true, Some(past)),
            "an expired entry must survive the read"
        );

        // The advisory caveat: expired, with no `expires_at` override. The deadline is `None`, so
        // it carries no liveness information -- but presence is still reported truthfully.
        store.cache_set(4, AdvisoryToken::stale_no_deadline(400));
        assert_eq!(
            store.cache_expires_at(&4u32),
            (true, None),
            "an expired value with no expires_at override must read (true, None): present, \
             deadline unknown -- never absent, and never proof of liveness"
        );
        assert_eq!(
            store.cache_peek_with_expiry_status(&4u32),
            (Some(AdvisoryToken::stale_no_deadline(400)), true),
            "is_expired must remain the liveness authority even though the deadline read is None"
        );
        assert_eq!(
            store.expires_at(&4u32),
            (true, None),
            "the alias must agree with the required method"
        );
    }

    /// Concurrent counterpart of [`assert_four_shapes_and_advisory_caveat_expires_at`].
    fn assert_four_shapes_and_advisory_caveat_expires_at_concurrent<C>(store: C)
    where
        C: ConcurrentCached<u32, AdvisoryToken>
            + ConcurrentCacheExpiry<u32, AdvisoryToken>
            + ConcurrentCloneCached<u32, AdvisoryToken>,
    {
        assert_eq!(store.cache_expires_at(&404u32), (false, None));

        let _ = store.cache_set(1, AdvisoryToken::live(100)).unwrap();
        assert_eq!(store.cache_expires_at(&1u32), (true, None));

        let future = Instant::now() + Duration::from_secs(60);
        let _ = store
            .cache_set(2, AdvisoryToken::live_with_deadline(200, future))
            .unwrap();
        assert_eq!(store.cache_expires_at(&2u32), (true, Some(future)));

        let past = Instant::now() - Duration::from_millis(50);
        let _ = store
            .cache_set(3, AdvisoryToken::stale_with_deadline(300, past))
            .unwrap();
        assert_eq!(store.cache_expires_at(&3u32), (true, Some(past)));
        assert_eq!(
            store.cache_expires_at(&3u32),
            (true, Some(past)),
            "an expired entry must survive the read"
        );

        let _ = store
            .cache_set(4, AdvisoryToken::stale_no_deadline(400))
            .unwrap();
        assert_eq!(
            store.cache_expires_at(&4u32),
            (true, None),
            "an expired value with no expires_at override must read (true, None): present, \
             deadline unknown -- never absent, and never proof of liveness"
        );
        assert_eq!(
            store.cache_peek_with_expiry_status(&4u32),
            (Some(AdvisoryToken::stale_no_deadline(400)), true),
            "is_expired must remain the liveness authority even though the deadline read is None"
        );
        assert_eq!(
            store.expires_at(&4u32),
            (true, None),
            "the alias must agree with the required method"
        );
    }

    #[test]
    fn expires_at_four_shapes_and_advisory_caveat_uniform_across_single_owner_expires_stores() {
        assert_four_shapes_and_advisory_caveat_expires_at(
            ExpiringCache::<u32, AdvisoryToken>::builder()
                .build()
                .unwrap(),
        );
        assert_four_shapes_and_advisory_caveat_expires_at(
            ExpiringLruCache::<u32, AdvisoryToken>::builder()
                .max_size(8)
                .build()
                .unwrap(),
        );
    }

    #[test]
    fn expires_at_four_shapes_and_advisory_caveat_uniform_across_sharded_expires_stores() {
        assert_four_shapes_and_advisory_caveat_expires_at_concurrent(
            ShardedExpiringCache::<u32, AdvisoryToken>::builder()
                .build()
                .unwrap(),
        );
        assert_four_shapes_and_advisory_caveat_expires_at_concurrent(
            ShardedExpiringLruCache::<u32, AdvisoryToken>::builder()
                .shards(1)
                .per_shard_max_size(8)
                .build()
                .unwrap(),
        );
    }

    /// Drives [`assert_reads_agree_single_owner`] over a key in every quadrant, including the
    /// advisory-caveat one (expired, no override). Each key's quadrant is asserted first, so
    /// agreement cannot pass vacuously.
    fn assert_reads_agree_expires<C>(mut store: C)
    where
        C: Cached<u32, AdvisoryToken> + CacheExpiry<u32, AdvisoryToken>,
    {
        assert_eq!(store.cache_expires_at(&404u32), (false, None));
        assert_reads_agree_single_owner(&store, &404u32);

        store.cache_set(1, AdvisoryToken::live(100));
        assert_eq!(store.cache_expires_at(&1u32), (true, None));
        assert_reads_agree_single_owner(&store, &1u32);

        let future = Instant::now() + Duration::from_secs(60);
        store.cache_set(2, AdvisoryToken::live_with_deadline(200, future));
        assert_eq!(store.cache_expires_at(&2u32), (true, Some(future)));
        assert_reads_agree_single_owner(&store, &2u32);

        let past = Instant::now() - Duration::from_millis(50);
        store.cache_set(3, AdvisoryToken::stale_with_deadline(300, past));
        assert_eq!(store.cache_expires_at(&3u32), (true, Some(past)));
        assert_reads_agree_single_owner(&store, &3u32);

        store.cache_set(4, AdvisoryToken::stale_no_deadline(400));
        assert_eq!(store.cache_expires_at(&4u32), (true, None));
        assert_reads_agree_single_owner(&store, &4u32);
    }

    /// Concurrent counterpart of [`assert_reads_agree_expires`].
    fn assert_reads_agree_expires_concurrent<C>(store: C)
    where
        C: ConcurrentCached<u32, AdvisoryToken> + ConcurrentCacheExpiry<u32, AdvisoryToken>,
    {
        assert_eq!(store.cache_expires_at(&404u32), (false, None));
        assert_reads_agree_concurrent(&store, &404u32);

        let _ = store.cache_set(1, AdvisoryToken::live(100)).unwrap();
        assert_eq!(store.cache_expires_at(&1u32), (true, None));
        assert_reads_agree_concurrent(&store, &1u32);

        let future = Instant::now() + Duration::from_secs(60);
        let _ = store
            .cache_set(2, AdvisoryToken::live_with_deadline(200, future))
            .unwrap();
        assert_eq!(store.cache_expires_at(&2u32), (true, Some(future)));
        assert_reads_agree_concurrent(&store, &2u32);

        let past = Instant::now() - Duration::from_millis(50);
        let _ = store
            .cache_set(3, AdvisoryToken::stale_with_deadline(300, past))
            .unwrap();
        assert_eq!(store.cache_expires_at(&3u32), (true, Some(past)));
        assert_reads_agree_concurrent(&store, &3u32);

        let _ = store
            .cache_set(4, AdvisoryToken::stale_no_deadline(400))
            .unwrap();
        assert_eq!(store.cache_expires_at(&4u32), (true, None));
        assert_reads_agree_concurrent(&store, &4u32);
    }

    #[test]
    fn the_two_reads_agree_uniform_across_single_owner_expires_stores() {
        assert_reads_agree_expires(
            ExpiringCache::<u32, AdvisoryToken>::builder()
                .build()
                .unwrap(),
        );
        assert_reads_agree_expires(
            ExpiringLruCache::<u32, AdvisoryToken>::builder()
                .max_size(8)
                .build()
                .unwrap(),
        );
    }

    #[test]
    fn the_two_reads_agree_uniform_across_sharded_expires_stores() {
        assert_reads_agree_expires_concurrent(
            ShardedExpiringCache::<u32, AdvisoryToken>::builder()
                .build()
                .unwrap(),
        );
        assert_reads_agree_expires_concurrent(
            ShardedExpiringLruCache::<u32, AdvisoryToken>::builder()
                .shards(1)
                .per_shard_max_size(8)
                .build()
                .unwrap(),
        );
    }

    #[test]
    fn expires_at_side_effect_free_uniform_across_single_owner_expires_stores() {
        let mut plain: ExpiringCache<u32, AdvisoryToken> =
            ExpiringCache::builder().build().unwrap();
        plain.cache_set(1, AdvisoryToken::live(100));
        assert_expires_at_side_effect_free_single_owner(&mut plain, &1u32, &999u32);

        let mut lru: ExpiringLruCache<u32, AdvisoryToken> =
            ExpiringLruCache::builder().max_size(8).build().unwrap();
        lru.cache_set(1, AdvisoryToken::live(100));
        assert_expires_at_side_effect_free_single_owner(&mut lru, &1u32, &999u32);
    }

    #[test]
    fn expires_at_side_effect_free_uniform_across_sharded_expires_stores() {
        let plain: ShardedExpiringCache<u32, AdvisoryToken> =
            ShardedExpiringCache::builder().build().unwrap();
        let _ = plain.cache_set(1, AdvisoryToken::live(100)).unwrap();
        assert_expires_at_side_effect_free_concurrent(&plain, &1u32, &999u32);

        let lru: ShardedExpiringLruCache<u32, AdvisoryToken> = ShardedExpiringLruCache::builder()
            .shards(1)
            .per_shard_max_size(8)
            .build()
            .unwrap();
        let _ = lru.cache_set(1, AdvisoryToken::live(100)).unwrap();
        assert_expires_at_side_effect_free_concurrent(&lru, &1u32, &999u32);
    }

    #[test]
    fn expires_at_lru_no_promotion_uniform_across_single_owner_expires_lru_store() {
        let store: ExpiringLruCache<u32, AdvisoryToken> =
            ExpiringLruCache::builder().max_size(2).build().unwrap();
        assert_expires_at_does_not_promote_lru_single_owner(
            store,
            1u32,
            AdvisoryToken::live(100),
            2u32,
            AdvisoryToken::live(200),
            3u32,
            AdvisoryToken::live(300),
        );
    }

    #[test]
    fn expires_at_lru_no_promotion_uniform_across_sharded_expires_lru_store() {
        let store: ShardedExpiringLruCache<u32, AdvisoryToken> = ShardedExpiringLruCache::builder()
            .shards(1)
            .per_shard_max_size(2)
            .build()
            .unwrap();
        assert_expires_at_does_not_promote_lru_concurrent(
            store,
            1u32,
            AdvisoryToken::live(100),
            2u32,
            AdvisoryToken::live(200),
            3u32,
            AdvisoryToken::live(300),
        );
    }

    /// The value-free counterpart of
    /// [`peek_expires_at_accepts_a_borrowed_key_on_single_owner_expires_stores`]: `Q = str` with
    /// `Q != K`. Run in the ungated module, so the borrowed-key path is certified for
    /// `cache_expires_at` even with zero features enabled.
    #[test]
    fn expires_at_accepts_a_borrowed_key_on_single_owner_expires_stores() {
        let deadline = Instant::now() + Duration::from_secs(60);

        let mut plain: ExpiringCache<String, AdvisoryToken> =
            ExpiringCache::builder().build().unwrap();
        plain.cache_set(
            "k".to_string(),
            AdvisoryToken::live_with_deadline(1, deadline),
        );
        assert_eq!(
            expires_at_via_borrowed_str(&plain, "k"),
            (true, Some(deadline)),
            "a &str read must reach the same entry a &String read does"
        );
        assert_eq!(
            expires_at_via_borrowed_str(&plain, "k"),
            plain.cache_expires_at(&"k".to_string()),
            "the borrowed and owned key forms must not diverge"
        );
        assert_eq!(
            plain.expires_at("k"),
            plain.cache_expires_at("k"),
            "the alias must accept the borrowed key too, and agree"
        );
        assert_eq!(
            expires_at_via_borrowed_str::<AdvisoryToken, _>(&plain, "absent"),
            (false, None)
        );

        let mut lru: ExpiringLruCache<String, AdvisoryToken> =
            ExpiringLruCache::builder().max_size(8).build().unwrap();
        lru.cache_set(
            "k".to_string(),
            AdvisoryToken::live_with_deadline(1, deadline),
        );
        assert_eq!(
            expires_at_via_borrowed_str(&lru, "k"),
            (true, Some(deadline))
        );
        assert_eq!(
            expires_at_via_borrowed_str::<AdvisoryToken, _>(&lru, "absent"),
            (false, None)
        );
    }

    /// The value-free counterpart of
    /// [`peek_expires_at_accepts_a_borrowed_slice_key_on_single_owner_expires_stores`]:
    /// `K = Vec<u8>` read through `Q = [u8]`, a `Borrow` pair unrelated to `String`/`str`.
    #[test]
    fn expires_at_accepts_a_borrowed_slice_key_on_single_owner_expires_stores() {
        let deadline = Instant::now() + Duration::from_secs(60);

        let mut plain: ExpiringCache<Vec<u8>, AdvisoryToken> =
            ExpiringCache::builder().build().unwrap();
        plain.cache_set(
            vec![1, 2, 3],
            AdvisoryToken::live_with_deadline(7, deadline),
        );
        assert_eq!(
            expires_at_via_borrowed_slice(&plain, &[1, 2, 3]),
            (true, Some(deadline)),
            "a &[u8] read must reach the same entry a &Vec<u8> read does"
        );
        assert_eq!(
            expires_at_via_borrowed_slice(&plain, &[1, 2, 3]),
            plain.cache_expires_at(&vec![1, 2, 3]),
            "the borrowed and owned key forms must not diverge"
        );
        assert_eq!(
            expires_at_via_borrowed_slice::<AdvisoryToken, _>(&plain, &[9, 9, 9]),
            (false, None)
        );
    }

    /// The borrowed-key form combined with LRU-no-promotion, matching
    /// [`peek_via_borrowed_key_does_not_promote_lru_on_single_owner_expires_lru_store`].
    #[test]
    fn expires_at_via_borrowed_key_does_not_promote_lru_on_single_owner_expires_lru_store() {
        let mut lru: ExpiringLruCache<String, AdvisoryToken> =
            ExpiringLruCache::builder().max_size(2).build().unwrap();
        lru.cache_set("k1".to_string(), AdvisoryToken::live(100)); // LRU
        lru.cache_set("k2".to_string(), AdvisoryToken::live(200)); // MRU

        for _ in 0..3 {
            assert_eq!(expires_at_via_borrowed_str(&lru, "k1"), (true, None));
        }

        lru.cache_set("k3".to_string(), AdvisoryToken::live(300));

        assert_eq!(
            lru.cache_get(&"k1".to_string()),
            None,
            "a borrowed-key deadline read must not promote recency; k1 must still be the victim"
        );
        assert_eq!(
            lru.cache_get(&"k2".to_string()),
            Some(&AdvisoryToken::live(200))
        );
        assert_eq!(
            lru.cache_get(&"k3".to_string()),
            Some(&AdvisoryToken::live(300))
        );
    }

    #[test]
    fn cache_expires_at_reachable_via_prelude_and_generic_bound_expires_stores() {
        let mut plain: ExpiringCache<u32, AdvisoryToken> =
            ExpiringCache::builder().build().unwrap();
        plain.cache_set(1, AdvisoryToken::live(100));
        assert_eq!(expires_at_via_generic_bound(&plain, &1u32), (true, None));

        let mut lru: ExpiringLruCache<u32, AdvisoryToken> =
            ExpiringLruCache::builder().max_size(8).build().unwrap();
        lru.cache_set(1, AdvisoryToken::live(100));
        assert_eq!(expires_at_via_generic_bound(&lru, &1u32), (true, None));
    }

    #[test]
    fn concurrent_cache_expires_at_reachable_via_prelude_and_generic_bound_expires_stores() {
        let plain: ShardedExpiringCache<u32, AdvisoryToken> =
            ShardedExpiringCache::builder().build().unwrap();
        let _ = plain.cache_set(1, AdvisoryToken::live(100)).unwrap();
        assert_eq!(
            expires_at_via_generic_bound_concurrent(&plain, &1u32),
            (true, None)
        );

        let lru: ShardedExpiringLruCache<u32, AdvisoryToken> = ShardedExpiringLruCache::builder()
            .shards(1)
            .per_shard_max_size(8)
            .build()
            .unwrap();
        let _ = lru.cache_set(1, AdvisoryToken::live(100)).unwrap();
        assert_eq!(
            expires_at_via_generic_bound_concurrent(&lru, &1u32),
            (true, None)
        );
    }

    /// Property 8 on the `Expires` family: a deadline is readable for a value type that does not
    /// implement `Clone`. `Expires` itself requires no `Clone`, so `NotCloneToken` is a legal
    /// value type for these stores, and `cache_peek_expires_at` cannot be called for it at all.
    ///
    /// As in the TTL-family counterpart, the sharded stores are reached through their absent-key
    /// path only: inserting through `ConcurrentCached` requires `V: Clone`.
    #[test]
    fn expires_at_reads_a_deadline_for_a_non_clone_value_on_expires_stores() {
        let deadline = Instant::now() + Duration::from_secs(60);

        let mut plain: ExpiringCache<u32, NotCloneToken> =
            ExpiringCache::builder().build().unwrap();
        plain.cache_set(
            1,
            NotCloneToken {
                expired: false,
                deadline: Some(deadline),
            },
        );
        assert_eq!(
            expires_at_via_generic_bound(&plain, &1u32),
            (true, Some(deadline)),
            "the deadline must be readable without cloning the value"
        );
        assert_eq!(expires_at_via_generic_bound(&plain, &999u32), (false, None));

        // The caveat, on a non-`Clone` value: expired, no deadline recorded -> (true, None).
        plain.cache_set(
            2,
            NotCloneToken {
                expired: true,
                deadline: None,
            },
        );
        assert_eq!(expires_at_via_generic_bound(&plain, &2u32), (true, None));

        let mut lru: ExpiringLruCache<u32, NotCloneToken> =
            ExpiringLruCache::builder().max_size(8).build().unwrap();
        lru.cache_set(
            1,
            NotCloneToken {
                expired: false,
                deadline: Some(deadline),
            },
        );
        assert_eq!(
            expires_at_via_generic_bound(&lru, &1u32),
            (true, Some(deadline))
        );

        let sharded: ShardedExpiringCache<u32, NotCloneToken> =
            ShardedExpiringCache::builder().build().unwrap();
        assert_eq!(
            expires_at_via_generic_bound_concurrent(&sharded, &1u32),
            (false, None)
        );

        let sharded_lru: ShardedExpiringLruCache<u32, NotCloneToken> =
            ShardedExpiringLruCache::builder()
                .shards(1)
                .per_shard_max_size(8)
                .build()
                .unwrap();
        assert_eq!(
            expires_at_via_generic_bound_concurrent(&sharded_lru, &1u32),
            (false, None)
        );
    }
}
