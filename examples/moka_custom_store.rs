/*
Using `moka` as a custom store for `#[concurrent_cached]` (issue #220).

`moka::sync::Cache` cannot be handed to `#[concurrent_cached(ty = ..., create = ...)]`
directly. Two independent blockers, both verified against moka 0.12.16:

  1. The orphan rule. `ConcurrentCached` and `moka::sync::Cache` are both foreign to
     your crate, so `impl cached::ConcurrentCached<K, V> for moka::sync::Cache<K, V>`
     is rejected with E0117 ("only traits defined in the current crate can be
     implemented for types defined outside of the crate"). This alone settles it: a
     local newtype is mandatory, not a stylistic choice.
  2. Even ignoring (1), four of the seven required methods do not line up:
        cache_set          wants `Option<V>`; `Cache::insert` returns `()`
        cache_remove       wants `Option<V>`; `Cache::invalidate` returns `()`
                           (`Cache::remove` is the one that returns `Option<V>`)
        cache_remove_entry wants `Option<(K, V)>`; moka has no `remove_entry`
        cache_clear        `Cache::invalidate_all` is lazy, not an eager clear
     `cache_get` / `cache_contains` map cleanly onto `get` / `contains_key`.

So the answer is a newtype. `MokaStore<K, V>` below is the whole adapter: the seven
required `ConcurrentCached` methods plus `type Error` and two optional overrides.
Everything else on the trait family (`cache_delete`, `cache_get_or_set_with`,
`metrics`, the `ConcurrentCachedExt` short names) is defaulted or blanket-impl'd.

Bounds a user ends up carrying: `K: Hash + Eq + Clone + Send + Sync + 'static`
(moka wants all but `Clone`; `cache_remove_entry` adds `Clone`) and
`V: Clone + Send + Sync + 'static` (moka wants all of these anyway).

Not covered here: `moka::future::Cache`. That needs `ConcurrentCachedAsync`, which is
a different trait with `async_`-prefixed methods; see the note at the bottom of this
file.

Run:
    cargo run --example moka_custom_store --features proc_macro
*/

use cached::macros::concurrent_cached;
use cached::{ConcurrentCacheBase, ConcurrentCached, ConcurrentCachedExt};
use moka::ops::compute::Op;
use moka::sync::Cache;
use std::convert::Infallible;
use std::hash::Hash;
use std::sync::atomic::{AtomicUsize, Ordering};

// ============================================================================
// The adapter
// ============================================================================

/// Newtype over `moka::sync::Cache`. The newtype exists to get around the orphan
/// rule (see the header); it holds no state of its own.
pub struct MokaStore<K, V> {
    inner: Cache<K, V>,
}

impl<K, V> MokaStore<K, V>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    pub fn new(max_capacity: u64) -> Self {
        Self {
            inner: Cache::new(max_capacity),
        }
    }
}

impl<K, V> ConcurrentCacheBase for MokaStore<K, V>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    // moka's sync operations do not fail, so nothing here can produce an error.
    // The macro still emits `?` / `map_error` on every cache op for a custom `ty`
    // (only the built-in sharded stores take the infallible codegen path), which
    // is why the cached functions below return `Result<_, Infallible>`.
    type Error = Infallible;

    /// `entry_count` is an estimate: writes and evictions are queued and applied by
    /// a maintenance pass, so a freshly-written entry may not be counted yet. Calling
    /// `run_pending_tasks` first drains that queue, which makes the number exact at the
    /// moment of the call at the cost of doing the maintenance work inline. Reporting
    /// the raw `entry_count()` instead would be cheaper and wrong right after a burst
    /// of writes.
    fn cache_size(&self) -> Result<Option<usize>, Self::Error> {
        self.inner.run_pending_tasks();
        Ok(Some(
            usize::try_from(self.inner.entry_count()).unwrap_or(usize::MAX),
        ))
    }

    fn cache_capacity(&self) -> Option<usize> {
        self.inner
            .policy()
            .max_capacity()
            .map(|c| usize::try_from(c).unwrap_or(usize::MAX))
    }

    // `cache_hits` / `cache_misses` / `cache_evictions` stay at their `None` defaults:
    // moka keeps no such counters. `metrics()` therefore reports entry count and
    // capacity only, and `hit_ratio()` is `None`.
}

impl<K, V> ConcurrentCached<K, V> for MokaStore<K, V>
where
    K: Hash + Eq + Clone + Send + Sync + 'static,
    V: Clone + Send + Sync + 'static,
{
    fn cache_get(&self, k: &K) -> Result<Option<V>, Self::Error> {
        Ok(self.inner.get(k))
    }

    /// `Cache::insert` returns `()`, so the previous value has to come from somewhere
    /// else. The naive fix is `let prev = self.inner.get(k); self.inner.insert(k, v);`,
    /// which is racy: a concurrent writer can land between the get and the insert, and
    /// this call then reports a value that was already overwritten.
    ///
    /// `entry(k).and_compute_with(..)` avoids the race outright. moka holds the per-key
    /// lock across the closure, so the `Option<Entry<K, V>>` handed to it is the value
    /// this write is actually replacing, and `Op::Put` is applied under the same lock.
    /// No get-then-set window exists.
    fn cache_set(&self, k: K, v: V) -> Result<Option<V>, Self::Error> {
        let mut prev = None;
        self.inner.entry(k).and_compute_with(|entry| {
            prev = entry.map(moka::Entry::into_value);
            Op::Put(v)
        });
        Ok(prev)
    }

    /// `Cache::remove`, not `Cache::invalidate`. Both drop the entry; only `remove`
    /// returns a clone of the discarded value, which is what this method's signature
    /// requires. Reaching for `invalidate` here (the more discoverable name) silently
    /// loses the return value and fails to compile against the trait.
    fn cache_remove(&self, k: &K) -> Result<Option<V>, Self::Error> {
        Ok(self.inner.remove(k))
    }

    /// moka has no `remove_entry`. The stored key is not reachable as an owned value
    /// (`Entry::key()` yields `&K`), so the returned key is a clone of the caller's.
    /// That is what forces `K: Clone` into every bound list in this file; moka itself
    /// never asks for it.
    fn cache_remove_entry(&self, k: &K) -> Result<Option<(K, V)>, Self::Error> {
        Ok(self.inner.remove(k).map(|v| (k.clone(), v)))
    }

    /// `invalidate_all` is lazy. It stamps an invalidation time and returns; the entries
    /// are dropped later by a maintenance pass. What a caller observes immediately after
    /// is still correct - moka guarantees `get` will not return anything inserted at or
    /// before the stamp - but a size read is not. `cache_size` above runs
    /// `run_pending_tasks` for that reason, and `main` asserts on reads rather than on
    /// the count.
    fn cache_clear(&self) -> Result<(), Self::Error> {
        self.inner.invalidate_all();
        Ok(())
    }

    /// Same as `cache_clear`: there are no metrics to reset, since moka tracks none.
    fn cache_reset(&self) -> Result<(), Self::Error> {
        self.inner.invalidate_all();
        Ok(())
    }

    fn cache_contains(&self, k: &K) -> Result<bool, Self::Error> {
        Ok(self.inner.contains_key(k))
    }
}

// ============================================================================
// Cached functions backed by the adapter
// ============================================================================

static CALLS: AtomicUsize = AtomicUsize::new(0);
static EVICTING_CALLS: AtomicUsize = AtomicUsize::new(0);

// A custom `ty` always takes the fallible codegen path, so the cached function must return
// a `Result` even though this store cannot fail. `map_error = "|e| e"` is the identity
// conversion from the store's `Infallible` to the function's own error type. It is optional:
// omitting it (see `bounded` below) makes the macro emit a bare `?`, which needs only
// `E: From<StoreError>`. A function with its own error type needs one or the other.
#[concurrent_cached(
    ty = "MokaStore<u64, String>",
    create = "{ MokaStore::new(1_000) }",
    map_error = "|e| e"
)]
fn expensive(n: u64) -> Result<String, Infallible> {
    CALLS.fetch_add(1, Ordering::SeqCst);
    Ok(format!("value-{n}"))
}

// A deliberately tiny bound so moka's eviction is observable. No `map_error` here: the
// generated `?` resolves through the reflexive `impl From<Infallible> for Infallible`.
#[concurrent_cached(
    name = "BOUNDED",
    ty = "MokaStore<u64, u64>",
    create = "{ MokaStore::new(4) }"
)]
fn bounded(n: u64) -> Result<u64, Infallible> {
    EVICTING_CALLS.fetch_add(1, Ordering::SeqCst);
    Ok(n * 2)
}

fn main() {
    // ========================================================================
    // 1. A hit does not re-run the body
    // ========================================================================
    assert_eq!(CALLS.load(Ordering::SeqCst), 0);
    let a = expensive(7).expect("infallible");
    let b = expensive(7).expect("infallible");
    assert_eq!(a, "value-7");
    assert_eq!(a, b);
    assert_eq!(
        CALLS.load(Ordering::SeqCst),
        1,
        "second call must be served from moka, not recomputed"
    );
    println!(
        "expensive(7) = {a:?}, body ran {} time(s)",
        CALLS.load(Ordering::SeqCst)
    );

    // ========================================================================
    // 2. moka's capacity bound really evicts, through the macro-generated static
    //
    // 100 distinct keys into a cache bounded at 4. Which keys survive is up to
    // moka's admission policy, so assert on the population rather than on a
    // specific key.
    // ========================================================================
    for n in 0..100u64 {
        assert_eq!(bounded(n).expect("infallible"), n * 2);
    }
    assert_eq!(EVICTING_CALLS.load(Ordering::SeqCst), 100);

    let size = BOUNDED
        .cache_size()
        .expect("infallible")
        .expect("moka reports a count");
    assert!(
        size <= 4,
        "entry_count after run_pending_tasks was {size}, bound is 4"
    );
    assert_eq!(BOUNDED.cache_capacity(), Some(4));

    let survivors = (0..100u64)
        .filter(|n| BOUNDED.contains(n).expect("infallible"))
        .count();
    assert!(
        survivors <= 4,
        "{survivors} of 100 keys survived a max_capacity of 4"
    );
    assert!(survivors < 100, "nothing was evicted");
    println!("bounded: entry_count = {size}, {survivors}/100 keys still resident");

    // Re-running the evicted keys re-executes the body, which is the observable
    // consequence of the eviction.
    for n in 0..100u64 {
        let _ = bounded(n).expect("infallible");
    }
    let reruns = EVICTING_CALLS.load(Ordering::SeqCst) - 100;
    assert!(reruns >= 96, "expected >= 96 recomputes, got {reruns}");
    println!("bounded: {reruns}/100 keys recomputed after eviction");

    // ========================================================================
    // 3. The documented mismatch behavior, exercised directly
    // ========================================================================
    let store: MokaStore<String, u32> = MokaStore::new(16);

    // cache_set returns the value it replaced, read under moka's per-key lock.
    assert_eq!(store.set("k".to_string(), 1).expect("infallible"), None);
    assert_eq!(store.set("k".to_string(), 2).expect("infallible"), Some(1));
    assert_eq!(store.get(&"k".to_string()).expect("infallible"), Some(2));
    println!("set(k, 2) returned the replaced value Some(1)");

    // cache_remove_entry returns a clone of the caller's key, not the stored one.
    assert_eq!(
        store.remove_entry(&"k".to_string()).expect("infallible"),
        Some(("k".to_string(), 2))
    );
    assert_eq!(store.get(&"k".to_string()).expect("infallible"), None);
    assert_eq!(store.remove(&"k".to_string()).expect("infallible"), None);

    // cache_clear is lazy in moka, but reads are correct immediately.
    store.set("a".to_string(), 1).expect("infallible");
    store.set("b".to_string(), 2).expect("infallible");
    store.clear().expect("infallible");
    assert_eq!(store.get(&"a".to_string()).expect("infallible"), None);
    assert_eq!(store.get(&"b".to_string()).expect("infallible"), None);
    assert!(!store.contains(&"a".to_string()).expect("infallible"));
    println!("clear(): reads return None immediately (the count may lag)");

    // The defaulted trait methods work off the seven implemented ones for free.
    assert_eq!(
        store
            .get_or_set_with("z".to_string(), || 9)
            .expect("infallible"),
        9
    );
    assert!(store.delete(&"z".to_string()).expect("infallible"));
    assert!(!store.delete(&"z".to_string()).expect("infallible"));

    // metrics() reports what moka can answer: a count and a capacity, no hit/miss
    // counters.
    let m = store.metrics();
    assert_eq!(m.hits, None);
    assert_eq!(m.misses, None);
    assert_eq!(m.capacity, Some(16));
    println!("metrics: {m:?}");

    // ========================================================================
    // 4. Future work: the async path
    //
    // `moka::future::Cache` would need `ConcurrentCachedAsync`, a separate trait
    // whose methods are `async_`-prefixed (`async_cache_get`, `async_cache_set`,
    // ...) and return `impl Future<Output = ...> + Send`. The adapter above cannot
    // be reused: the method names differ, so it is a second impl block, and every
    // body must be `async move { ... }` over moka's `.await`-ing equivalents. The
    // `and_compute_with` trick for `cache_set` has a future counterpart
    // (`and_compute_with` on the async entry selector is itself async), so the
    // previous-value semantics should carry over. Not attempted here.
    // ========================================================================

    println!("\ndone!");
}
