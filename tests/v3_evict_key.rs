//! Regression tests for C1/C8: on an expired-entry replacement via
//! `cache_get_or_set_with_mut` / `cache_try_get_or_set_with_mut`, the `on_evict`
//! callback must receive the STORED key of the displaced entry, not the (equal but
//! distinct) lookup key.
//!
//! Both TTL-bounded LRU stores exercised here (`LruTtlCache`, `ExpiringLruCache`)
//! previously cloned the lookup key up front and passed it to `on_evict`. For key
//! types whose `Eq`/`Hash` admit distinct-but-equal instances (case-insensitive
//! keys, interned vs owned) that handed the callback the wrong instance. The stores
//! now thread the displaced entry's own key through from the inner `LruCache`.
//!
//! Gated on `time_stores` because both stores live behind that feature.
#![cfg(feature = "time_stores")]

use cached::time::Duration;
use cached::{Cached, Expires, ExpiringLruCache, LruTtlCache};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

/// A case-insensitive string key: two `CiKey`s are equal (and hash equal) when their
/// contents match ignoring ASCII case, so `"Hello"` and `"HELLO"` are equal-but-distinct
/// instances. The callback must observe the exact bytes that were stored.
#[derive(Debug, Clone)]
struct CiKey(String);

impl PartialEq for CiKey {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(&other.0)
    }
}
impl Eq for CiKey {}
impl Hash for CiKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for b in self.0.as_bytes() {
            state.write_u8(b.to_ascii_lowercase());
        }
    }
}

/// Sanity: the two instances are equal and hash identically, but carry different bytes.
#[test]
fn ci_key_is_equal_but_distinct() {
    let stored = CiKey("Hello".into());
    let lookup = CiKey("HELLO".into());
    assert_eq!(stored, lookup, "case-insensitive keys must compare equal");
    assert_ne!(
        stored.0, lookup.0,
        "the underlying bytes must differ so the test can tell them apart"
    );
}

// ───────────────────────────── LruTtlCache ──────────────────────────────

#[test]
fn lru_ttl_get_or_set_evict_receives_stored_key() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);

    let mut cache: LruTtlCache<CiKey, u32> = LruTtlCache::builder()
        .max_size(8)
        .ttl(Duration::from_millis(100))
        .on_evict(move |k: &CiKey, _v: &u32| sink.lock().unwrap().push(k.0.clone()))
        .build()
        .expect("build LruTtlCache");

    // Store under the mixed-case key, let it expire.
    cache.cache_set(CiKey("Hello".into()), 1);
    std::thread::sleep(std::time::Duration::from_millis(250));

    // Look up the equal-but-distinct upper-case key; the expired entry is replaced and
    // on_evict must fire with the STORED key "Hello".
    let v = cache.cache_get_or_set_with_mut(CiKey("HELLO".into()), || 2);
    assert_eq!(*v, 2, "expired entry should be replaced by the factory value");

    let seen = seen.lock().unwrap();
    assert_eq!(
        &*seen,
        &["Hello".to_string()],
        "on_evict must receive the stored key `Hello`, not the lookup key `HELLO`"
    );
}

#[test]
fn lru_ttl_try_get_or_set_evict_receives_stored_key() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);

    let mut cache: LruTtlCache<CiKey, u32> = LruTtlCache::builder()
        .max_size(8)
        .ttl(Duration::from_millis(100))
        .on_evict(move |k: &CiKey, _v: &u32| sink.lock().unwrap().push(k.0.clone()))
        .build()
        .expect("build LruTtlCache");

    cache.cache_set(CiKey("Hello".into()), 1);
    std::thread::sleep(std::time::Duration::from_millis(250));

    let v = cache
        .cache_try_get_or_set_with_mut(CiKey("HELLO".into()), || Ok::<u32, ()>(2))
        .expect("factory succeeds");
    assert_eq!(*v, 2);

    let seen = seen.lock().unwrap();
    assert_eq!(
        &*seen,
        &["Hello".to_string()],
        "on_evict must receive the stored key `Hello`, not the lookup key `HELLO`"
    );
}

// ──────────────────────────── ExpiringLruCache ──────────────────────────

/// A value that decides its own staleness. Stored already-expired so the next
/// get-or-set replaces it and fires `on_evict`.
#[derive(Debug, Clone)]
struct Flag {
    expired: bool,
}
impl Expires for Flag {
    fn is_expired(&self) -> bool {
        self.expired
    }
}

#[test]
fn expiring_lru_get_or_set_evict_receives_stored_key() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);

    let mut cache: ExpiringLruCache<CiKey, Flag> = ExpiringLruCache::builder()
        .max_size(8)
        .on_evict(move |k: &CiKey, _v: &Flag| sink.lock().unwrap().push(k.0.clone()))
        .build()
        .expect("build ExpiringLruCache");

    // Store an already-expired value under the mixed-case key.
    cache.cache_set(CiKey("Hello".into()), Flag { expired: true });

    // Look up the equal-but-distinct upper-case key; the expired entry is replaced.
    let v = cache.cache_get_or_set_with_mut(CiKey("HELLO".into()), || Flag { expired: false });
    assert!(!v.is_expired(), "expired entry should be replaced");

    let seen = seen.lock().unwrap();
    assert_eq!(
        &*seen,
        &["Hello".to_string()],
        "on_evict must receive the stored key `Hello`, not the lookup key `HELLO`"
    );
}

#[test]
fn expiring_lru_try_get_or_set_evict_receives_stored_key() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);

    let mut cache: ExpiringLruCache<CiKey, Flag> = ExpiringLruCache::builder()
        .max_size(8)
        .on_evict(move |k: &CiKey, _v: &Flag| sink.lock().unwrap().push(k.0.clone()))
        .build()
        .expect("build ExpiringLruCache");

    cache.cache_set(CiKey("Hello".into()), Flag { expired: true });

    let v = cache
        .cache_try_get_or_set_with_mut(CiKey("HELLO".into()), || {
            Ok::<Flag, ()>(Flag { expired: false })
        })
        .expect("factory succeeds");
    assert!(!v.is_expired());

    let seen = seen.lock().unwrap();
    assert_eq!(
        &*seen,
        &["Hello".to_string()],
        "on_evict must receive the stored key `Hello`, not the lookup key `HELLO`"
    );
}

// ────────────── C10: miss-count parity across get-or-set variants ─────────────

/// Steady-state parity: on a fresh key both variants count exactly one miss and no hit,
/// invoking the factory once. (Passes before and after the C10 fix; guards the common
/// path stays consistent.)
#[test]
fn expiring_lru_miss_count_parity_between_variants() {
    // Infallible variant on a fresh key: one miss, factory called once.
    let mut c1: ExpiringLruCache<u32, Flag> =
        ExpiringLruCache::builder().max_size(8).build().unwrap();
    let mut calls = 0u32;
    let _ = c1.cache_get_or_set_with_mut(1, || {
        calls += 1;
        Flag { expired: false }
    });
    assert_eq!(calls, 1, "factory runs once on a miss");
    assert_eq!(c1.cache_misses(), Some(1));
    assert_eq!(c1.cache_hits(), Some(0));

    // Fallible variant on a fresh key: identical accounting.
    let mut c2: ExpiringLruCache<u32, Flag> =
        ExpiringLruCache::builder().max_size(8).build().unwrap();
    let mut calls2 = 0u32;
    let _ = c2
        .cache_try_get_or_set_with_mut::<_, ()>(1, || {
            calls2 += 1;
            Ok(Flag { expired: false })
        })
        .unwrap();
    assert_eq!(calls2, 1);
    assert_eq!(c2.cache_misses(), Some(1));
    assert_eq!(c2.cache_hits(), Some(0));
}

/// C10 divergence: the miss must be recorded even when the factory diverges (panics).
/// Before the fix the infallible variant counted the miss AFTER the factory returned,
/// so a panicking factory unwound before the count landed and the miss was lost; the
/// try variant already counted inside the factory wrapper. This pins that BOTH variants
/// now count the miss the instant the factory runs, so a panic no longer loses it.
#[test]
fn expiring_lru_panicking_factory_still_counts_miss_on_both_variants() {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    // Infallible variant: factory panics on a fresh key.
    let mut c1: ExpiringLruCache<u32, Flag> =
        ExpiringLruCache::builder().max_size(8).build().unwrap();
    let r = catch_unwind(AssertUnwindSafe(|| {
        let _ = c1.cache_get_or_set_with_mut(1, || -> Flag { panic!("boom") });
    }));
    assert!(r.is_err(), "factory panic should propagate");
    assert_eq!(
        c1.cache_misses(),
        Some(1),
        "infallible variant must count the miss even when the factory panics (C10)"
    );

    // Fallible variant: same, for parity.
    let mut c2: ExpiringLruCache<u32, Flag> =
        ExpiringLruCache::builder().max_size(8).build().unwrap();
    let r2 = catch_unwind(AssertUnwindSafe(|| {
        let _ = c2.cache_try_get_or_set_with_mut::<_, ()>(1, || -> Result<Flag, ()> {
            panic!("boom")
        });
    }));
    assert!(r2.is_err(), "factory panic should propagate");
    assert_eq!(
        c2.cache_misses(),
        Some(1),
        "fallible variant must count the miss even when the factory panics (C10)"
    );
}
