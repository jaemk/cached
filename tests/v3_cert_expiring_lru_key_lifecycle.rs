//! Certification of the *key lifecycle* half of the `ExpiringLruCache` perf change.
//!
//! Two claims are under test:
//!
//! 1. **The key clones are gone.** `cache_set` used to run `k.clone()` on every insert
//!    whenever an `on_evict` callback was configured, and `cache_clear_with_on_evict` used
//!    to clone every key via `key_order()`. Neither may clone a key any more.
//! 2. **Nothing observes a freed or half-moved key.** With the defensive clone removed,
//!    `on_evict` now borrows the entry's own key. These tests assert the callback sees a
//!    fully intact heap payload, that the key is still alive while the callback runs, and
//!    that it is dropped exactly once afterwards.
//!
//! `CountedKey` carries its own `Arc<AtomicUsize>` clone/drop counters, so the counts are
//! per-cache and the tests stay safe under the parallel test harness. The counters do not
//! participate in `Hash`/`Eq`; `id` alone does.

use cached::{CacheEvict, Cached, Expires, ExpiringLruCache};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
struct Counters {
    clones: AtomicUsize,
    drops: AtomicUsize,
}

impl Counters {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            clones: AtomicUsize::new(0),
            drops: AtomicUsize::new(0),
        })
    }
    fn clones(&self) -> usize {
        self.clones.load(Ordering::Relaxed)
    }
    fn drops(&self) -> usize {
        self.drops.load(Ordering::Relaxed)
    }
}

/// A key with a heap payload (`tag`) that is deliberately excluded from `Hash`/`Eq`, plus
/// instrumentation for clone and drop accounting.
#[derive(Debug)]
struct CountedKey {
    id: u32,
    tag: String,
    counters: Arc<Counters>,
}

impl CountedKey {
    fn new(id: u32, tag: &str, counters: &Arc<Counters>) -> Self {
        Self {
            id,
            tag: tag.to_string(),
            counters: Arc::clone(counters),
        }
    }
}

impl Clone for CountedKey {
    fn clone(&self) -> Self {
        self.counters.clones.fetch_add(1, Ordering::Relaxed);
        Self {
            id: self.id,
            tag: self.tag.clone(),
            counters: Arc::clone(&self.counters),
        }
    }
}

impl Drop for CountedKey {
    fn drop(&mut self) {
        self.counters.drops.fetch_add(1, Ordering::Relaxed);
    }
}

impl PartialEq for CountedKey {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for CountedKey {}
impl Hash for CountedKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

#[derive(Debug, Clone)]
struct Val {
    expired: bool,
}
impl Val {
    fn live() -> Self {
        Self { expired: false }
    }
    fn stale() -> Self {
        Self { expired: true }
    }
}
impl Expires for Val {
    fn is_expired(&self) -> bool {
        self.expired
    }
}

/// What the callback observed: the key's full heap payload and the drop count at the
/// moment it ran.
type Observations = Arc<Mutex<Vec<(String, usize)>>>;

fn cache_with_probe(
    max_size: usize,
    counters: &Arc<Counters>,
) -> (ExpiringLruCache<CountedKey, Val>, Observations) {
    let obs: Observations = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&obs);
    let counters = Arc::clone(counters);
    let cache = ExpiringLruCache::builder()
        .max_size(max_size)
        .on_evict(move |k: &CountedKey, _v: &Val| {
            // Reading `k.tag` here is the point: with the caller-key clone removed, this
            // borrows the entry's own key. A stale/dangling borrow would show up as a
            // wrong or corrupted payload (and would trip miri/ASAN).
            sink.lock()
                .unwrap()
                .push((k.tag.clone(), counters.drops.load(Ordering::Relaxed)));
        })
        .build()
        .expect("build ExpiringLruCache");
    (cache, obs)
}

fn observed(obs: &Observations) -> Vec<(String, usize)> {
    obs.lock().unwrap().clone()
}

/// The instrument must actually count, or every clone assertion below is vacuous.
#[test]
fn counted_key_counts_clones_and_drops() {
    let c = Counters::new();
    {
        let k = CountedKey::new(1, "a", &c);
        assert_eq!(c.clones(), 0);
        let k2 = k.clone();
        assert_eq!(c.clones(), 1, "Clone must be counted");
        assert_eq!(k, k2, "same id compares equal");
        assert_eq!(c.drops(), 0);
    }
    assert_eq!(c.drops(), 2, "both instances dropped at scope end");
}

// --- claim 1: no key clones ----------------------------------------------------------

#[test]
fn cache_set_of_new_keys_clones_no_key_even_with_on_evict_configured() {
    // The pre-change implementation ran `k.clone()` on EVERY cache_set while an on_evict
    // callback was configured. Three inserts must now cost zero key clones.
    let counters = Counters::new();
    let (mut c, obs) = cache_with_probe(8, &counters);
    c.cache_set(CountedKey::new(1, "a", &counters), Val::live());
    c.cache_set(CountedKey::new(2, "b", &counters), Val::live());
    c.cache_set(CountedKey::new(3, "c", &counters), Val::live());
    assert_eq!(
        counters.clones(),
        0,
        "cache_set must not clone the caller's key"
    );
    assert!(observed(&obs).is_empty(), "no evictions happened");
    assert_eq!(c.cache_size(), 3);
}

#[test]
fn cache_set_overwrite_clones_no_key_on_either_the_live_or_expired_branch() {
    let counters = Counters::new();
    let (mut c, _obs) = cache_with_probe(8, &counters);
    c.cache_set(CountedKey::new(1, "stored", &counters), Val::stale());
    // Expired branch: fires on_evict with the stored key.
    c.cache_set(CountedKey::new(1, "caller", &counters), Val::live());
    // Live branch: returns the displaced value, silent.
    c.cache_set(CountedKey::new(1, "third", &counters), Val::live());
    assert_eq!(counters.clones(), 0, "neither overwrite branch may clone");
}

#[test]
fn cache_set_capacity_eviction_clones_no_key() {
    let counters = Counters::new();
    let (mut c, obs) = cache_with_probe(1, &counters);
    c.cache_set(CountedKey::new(1, "victim", &counters), Val::live());
    c.cache_set(CountedKey::new(2, "fresh", &counters), Val::live());
    assert_eq!(counters.clones(), 0);
    assert_eq!(
        observed(&obs)
            .into_iter()
            .map(|(t, _)| t)
            .collect::<Vec<_>>(),
        vec!["victim".to_string()]
    );
}

#[test]
fn cache_get_lazy_sweep_clones_no_key() {
    let counters = Counters::new();
    let (mut c, obs) = cache_with_probe(8, &counters);
    c.cache_set(CountedKey::new(1, "stored", &counters), Val::stale());
    let probe = CountedKey::new(1, "lookup", &counters);
    assert!(c.cache_get(&probe).is_none());
    assert!(c.cache_get_mut(&probe).is_none(), "already swept");
    assert_eq!(counters.clones(), 0, "the lazy sweep must not clone a key");
    assert_eq!(
        observed(&obs)
            .into_iter()
            .map(|(t, _)| t)
            .collect::<Vec<_>>(),
        vec!["stored".to_string()]
    );
}

#[test]
fn cache_remove_entry_clones_no_key() {
    let counters = Counters::new();
    let (mut c, _obs) = cache_with_probe(8, &counters);
    c.cache_set(CountedKey::new(1, "stored", &counters), Val::live());
    let probe = CountedKey::new(1, "lookup", &counters);
    let (k, _) = c.cache_remove_entry(&probe).expect("present");
    assert_eq!(k.tag, "stored", "the stored key is moved out, not cloned");
    assert_eq!(counters.clones(), 0);
}

#[test]
fn cache_clear_with_on_evict_clones_no_key() {
    // The pre-change implementation collected `key_order()` (one clone per key) before
    // popping. `drain_all` must clone nothing.
    let counters = Counters::new();
    let (mut c, obs) = cache_with_probe(8, &counters);
    c.cache_set(CountedKey::new(1, "a", &counters), Val::live());
    c.cache_set(CountedKey::new(2, "b", &counters), Val::stale());
    c.cache_set(CountedKey::new(3, "c", &counters), Val::live());
    c.cache_clear_with_on_evict();
    assert_eq!(
        counters.clones(),
        0,
        "cache_clear_with_on_evict must not clone any key"
    );
    assert_eq!(
        observed(&obs)
            .into_iter()
            .map(|(t, _)| t)
            .collect::<Vec<_>>(),
        vec!["c".to_string(), "b".to_string(), "a".to_string()],
        "MRU -> LRU, expired entries included"
    );
    assert_eq!(c.cache_size(), 0);
}

#[test]
fn evict_and_retain_clone_no_key() {
    let counters = Counters::new();
    let (mut c, _obs) = cache_with_probe(8, &counters);
    c.cache_set(CountedKey::new(1, "a", &counters), Val::stale());
    c.cache_set(CountedKey::new(2, "b", &counters), Val::live());
    assert_eq!(CacheEvict::evict(&mut c), 1);
    let removed = c.retain(|_, _| false);
    assert_eq!(counters.clones(), 0, "sweeps must not clone keys");
    assert_eq!(c.cache_size(), 0);
    assert_eq!(
        removed, 1,
        "the one surviving live entry was rejected by keep"
    );
}

#[test]
fn set_max_size_shrink_clones_no_key() {
    let counters = Counters::new();
    let (mut c, _obs) = cache_with_probe(4, &counters);
    c.cache_set(CountedKey::new(1, "a", &counters), Val::live());
    c.cache_set(CountedKey::new(2, "b", &counters), Val::live());
    c.cache_set(CountedKey::new(3, "c", &counters), Val::live());
    let _ = c.set_max_size(1);
    assert_eq!(counters.clones(), 0);
    assert_eq!(c.cache_size(), 1);
}

#[test]
fn get_or_set_family_clones_no_key() {
    let counters = Counters::new();
    let (mut c, _obs) = cache_with_probe(8, &counters);
    c.cache_set(CountedKey::new(1, "stored", &counters), Val::stale());
    let _ = c.cache_get_or_set_with(CountedKey::new(1, "caller", &counters), Val::live);
    let r: Result<&Val, ()> =
        c.cache_try_get_or_set_with(CountedKey::new(2, "other", &counters), || Ok(Val::live()));
    assert!(r.is_ok());
    assert_eq!(counters.clones(), 0, "the get_or_set family must not clone");
}

// --- claim 2: the key handed to on_evict is intact and correctly owned ----------------

#[test]
fn on_evict_borrows_a_live_intact_key_on_the_cache_set_expired_branch() {
    let counters = Counters::new();
    let (mut c, obs) = cache_with_probe(8, &counters);
    c.cache_set(
        CountedKey::new(1, "stored-payload", &counters),
        Val::stale(),
    );
    assert_eq!(
        counters.drops(),
        0,
        "the inserted key was moved, not dropped"
    );

    c.cache_set(CountedKey::new(1, "caller-payload", &counters), Val::live());

    assert_eq!(
        observed(&obs),
        vec![("stored-payload".to_string(), 0)],
        "on_evict must see the STORED key's full payload, and the key must still be \
         alive (zero drops) while the callback runs"
    );
    assert_eq!(
        counters.drops(),
        1,
        "the displaced stored key is dropped exactly once, after the callback returns"
    );
    // The surviving entry is the caller's key; nothing else was dropped.
    let (k, _) = c
        .cache_remove_entry(&CountedKey::new(1, "probe", &counters))
        .expect("present");
    assert_eq!(k.tag, "caller-payload");
}

#[test]
fn on_evict_borrows_a_live_intact_key_on_the_capacity_path() {
    let counters = Counters::new();
    let (mut c, obs) = cache_with_probe(1, &counters);
    c.cache_set(CountedKey::new(1, "victim-payload", &counters), Val::live());
    c.cache_set(CountedKey::new(2, "fresh-payload", &counters), Val::live());
    assert_eq!(
        observed(&obs),
        vec![("victim-payload".to_string(), 0)],
        "the capacity victim's key must be intact and alive during the callback"
    );
    assert_eq!(counters.drops(), 1, "the victim key drops once, afterwards");
}

#[test]
fn on_evict_borrows_live_intact_keys_across_a_full_drain() {
    let counters = Counters::new();
    let (mut c, obs) = cache_with_probe(8, &counters);
    c.cache_set(CountedKey::new(1, "one", &counters), Val::live());
    c.cache_set(CountedKey::new(2, "two", &counters), Val::live());
    c.cache_clear_with_on_evict();
    assert_eq!(
        observed(&obs),
        vec![("two".to_string(), 0), ("one".to_string(), 0)],
        "drain_all holds every pair alive until all callbacks have run"
    );
    assert_eq!(counters.drops(), 2, "each drained key drops exactly once");
}

#[test]
fn every_key_is_dropped_exactly_once_over_a_mixed_workload() {
    // Cross-check for a leak or a double free hiding behind the removed clone: eight keys
    // enter the cache across every insert path, and after the cache is dropped the drop
    // count must equal the number of key instances ever created (8 inserted + 3 borrowed
    // probes), with zero clones.
    let counters = Counters::new();
    let (mut c, _obs) = cache_with_probe(3, &counters);

    c.cache_set(CountedKey::new(1, "k1", &counters), Val::live());
    c.cache_set(CountedKey::new(2, "k2", &counters), Val::stale());
    c.cache_set(CountedKey::new(3, "k3", &counters), Val::live());
    c.cache_set(CountedKey::new(4, "k4", &counters), Val::live()); // capacity eviction
    c.cache_set(CountedKey::new(2, "k2b", &counters), Val::live()); // over expired (if still held)
    let _ = c.cache_get_or_set_with(CountedKey::new(5, "k5", &counters), Val::live);
    let r: Result<&Val, ()> =
        c.cache_try_get_or_set_with(CountedKey::new(6, "k6", &counters), || Ok(Val::live()));
    assert!(r.is_ok());
    c.cache_set(CountedKey::new(7, "k7", &counters), Val::stale());

    {
        let p1 = CountedKey::new(3, "probe1", &counters);
        let p2 = CountedKey::new(7, "probe2", &counters);
        let p3 = CountedKey::new(99, "probe3", &counters);
        let _ = c.cache_get(&p1);
        let _ = c.cache_get(&p2);
        let _ = c.cache_get(&p3);
    }

    let _ = CacheEvict::evict(&mut c);
    let _ = c.retain(|_, _| true);
    drop(c);

    assert_eq!(counters.clones(), 0, "no path may clone a key");
    assert_eq!(
        counters.drops(),
        11,
        "every one of the 8 inserted + 3 probe key instances is dropped exactly once"
    );
}
