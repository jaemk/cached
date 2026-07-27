//! Regression test for the `ExpiringLruCache::cache_set` -> `cache_set_returning_entry`
//! switch (perf shard, item 5).
//!
//! Before the switch, `cache_set` cloned the CALLER's key up front (`k.clone()`) and
//! passed that clone to `on_evict` when the displaced entry was expired. After the
//! switch to `LruCache::cache_set_returning_entry`, `on_evict` receives the entry's own
//! STORED key instead -- matching the contract `cache_set_returning_entry` was written
//! for and already used by `LruTtlCache::set_entry` (see src/stores/lru.rs ~:716-750
//! and src/stores/lru_ttl.rs ~:366-384).
//!
//! This is only observable for key types whose `Eq`/`Hash` cover part of the payload:
//! two keys that compare equal (and hash equal) but carry different data. Here that is
//! an `id` field (covered by `Eq`/`Hash`) plus a `tag` field that is not. Insert under
//! one tag, overwrite with a different tag while the first entry is expired, and assert
//! the callback observes the FIRST-inserted (stored) tag, not the overwriting call's
//! tag. `LruTtlCache` is exercised the same way and both stores must agree.
//!
//! Gated on `time_stores` because `LruTtlCache` lives behind that feature.
#![cfg(feature = "time_stores")]

use cached::time::Duration;
use cached::{Cached, Expires, ExpiringLruCache, LruTtlCache};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

/// A key whose `Eq`/`Hash` cover only `id`; `tag` rides along uncompared, so two
/// `IdTagKey`s with the same `id` but different `tag` are equal-but-distinct instances.
#[derive(Debug, Clone)]
struct IdTagKey {
    id: u32,
    tag: &'static str,
}

impl PartialEq for IdTagKey {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for IdTagKey {}
impl Hash for IdTagKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

/// Sanity: the two instances compare equal and hash identically, but carry different
/// `tag`s so the test can distinguish which one the callback observed.
#[test]
fn id_tag_key_is_equal_but_distinct() {
    let stored = IdTagKey { id: 1, tag: "first" };
    let lookup = IdTagKey { id: 1, tag: "second" };
    assert_eq!(stored, lookup, "keys with the same id must compare equal");
    assert_ne!(
        stored.tag, lookup.tag,
        "tags must differ so the test can tell the two instances apart"
    );
}

/// A value that decides its own staleness via a simple flag.
#[derive(Debug, Clone)]
struct Flag {
    expired: bool,
}
impl Expires for Flag {
    fn is_expired(&self) -> bool {
        self.expired
    }
}

/// `ExpiringLruCache::cache_set`, overwriting an expired entry, must fire `on_evict`
/// with the STORED key ("first"), not the caller's overwriting key ("second").
#[test]
fn expiring_lru_cache_set_over_expired_on_evict_receives_stored_key() {
    let seen: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);

    let mut cache: ExpiringLruCache<IdTagKey, Flag> = ExpiringLruCache::builder()
        .max_size(8)
        .on_evict(move |k: &IdTagKey, _v: &Flag| sink.lock().unwrap().push(k.tag))
        .build()
        .expect("build ExpiringLruCache");

    // Store an already-expired value under the "first" tag.
    let none = cache.cache_set(
        IdTagKey {
            id: 1,
            tag: "first",
        },
        Flag { expired: true },
    );
    assert!(none.is_none(), "fresh insert returns None");

    // Overwrite with the equal-but-distinct "second"-tagged key. The displaced entry
    // was expired, so it is filtered from the return and on_evict fires once.
    let displaced = cache.cache_set(
        IdTagKey {
            id: 1,
            tag: "second",
        },
        Flag { expired: false },
    );
    assert!(
        displaced.is_none(),
        "an expired displaced value is filtered from cache_set's return"
    );

    let seen = seen.lock().unwrap();
    assert_eq!(
        &*seen,
        &["first"],
        "on_evict must receive the STORED key (tag \"first\"), not the overwriting \
         call's key (tag \"second\")"
    );
}

/// `LruTtlCache::cache_set`, overwriting an expired entry, must likewise fire
/// `on_evict` with the STORED key -- the contract `cache_set_returning_entry` was
/// written for, which `ExpiringLruCache::cache_set` now shares.
#[test]
fn lru_ttl_cache_set_over_expired_on_evict_receives_stored_key() {
    let seen: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);

    let mut cache: LruTtlCache<IdTagKey, u32> = LruTtlCache::builder()
        .max_size(8)
        .ttl(Duration::from_millis(100))
        .on_evict(move |k: &IdTagKey, _v: &u32| sink.lock().unwrap().push(k.tag))
        .build()
        .expect("build LruTtlCache");

    cache.cache_set(
        IdTagKey {
            id: 1,
            tag: "first",
        },
        10,
    );
    std::thread::sleep(std::time::Duration::from_millis(250));

    let displaced = cache.cache_set(
        IdTagKey {
            id: 1,
            tag: "second",
        },
        20,
    );
    assert!(
        displaced.is_none(),
        "an expired displaced value is filtered from cache_set's return"
    );

    let seen = seen.lock().unwrap();
    assert_eq!(
        &*seen,
        &["first"],
        "on_evict must receive the STORED key (tag \"first\"), not the overwriting \
         call's key (tag \"second\")"
    );
}

/// Both stores must agree: overwriting an expired entry under an equal-but-distinct
/// key fires `on_evict` with the first-inserted (stored) key's tag in both cases.
#[test]
fn expiring_lru_and_lru_ttl_agree_on_stored_key_for_cache_set_over_expired() {
    let expiring_seen: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let expiring_sink = Arc::clone(&expiring_seen);
    let mut expiring: ExpiringLruCache<IdTagKey, Flag> = ExpiringLruCache::builder()
        .max_size(8)
        .on_evict(move |k: &IdTagKey, _v: &Flag| expiring_sink.lock().unwrap().push(k.tag))
        .build()
        .expect("build ExpiringLruCache");
    expiring.cache_set(
        IdTagKey {
            id: 1,
            tag: "first",
        },
        Flag { expired: true },
    );
    expiring.cache_set(
        IdTagKey {
            id: 1,
            tag: "second",
        },
        Flag { expired: false },
    );

    let ttl_seen: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let ttl_sink = Arc::clone(&ttl_seen);
    let mut ttl: LruTtlCache<IdTagKey, u32> = LruTtlCache::builder()
        .max_size(8)
        .ttl(Duration::from_millis(100))
        .on_evict(move |k: &IdTagKey, _v: &u32| ttl_sink.lock().unwrap().push(k.tag))
        .build()
        .expect("build LruTtlCache");
    ttl.cache_set(
        IdTagKey {
            id: 1,
            tag: "first",
        },
        10,
    );
    std::thread::sleep(std::time::Duration::from_millis(250));
    ttl.cache_set(
        IdTagKey {
            id: 1,
            tag: "second",
        },
        20,
    );

    assert_eq!(
        &*expiring_seen.lock().unwrap(),
        &["first"],
        "ExpiringLruCache must fire on_evict with the stored key"
    );
    assert_eq!(
        &*ttl_seen.lock().unwrap(),
        &["first"],
        "LruTtlCache must fire on_evict with the stored key"
    );
    assert_eq!(
        &*expiring_seen.lock().unwrap(),
        &*ttl_seen.lock().unwrap(),
        "ExpiringLruCache and LruTtlCache must agree on which key on_evict observes"
    );
}
