/*!
Store-level certification for `CloneCached::cache_peek_with_expiry_status`.

This is the side-effect-free, expired-surfacing peek added to fix the bug where
`#[cached(result_fallback = true, force_refresh = "...")]` over a TTL store dropped
the stale `Ok` fallback when a bypassed recompute returned `Err` over an EXPIRED
entry (the bypass branch used `cache_peek`, which returns `None` for expired entries).

The macro end-to-end coverage lives in `v3_force_refresh.rs`. These tests pin the
trait method directly on every single-owner override, because the method has
contractual guarantees the macro tests cannot fully observe:

  1. Surfaces a present-but-expired entry as `(Some(v), true)` (the regression).
  2. Returns the live value as `(Some(v), false)` and absent keys as `(None, false)`.
  3. Produces NO read side effects: no hit/miss counter change, no LRU recency
     promotion, no TTL renewal.
  4. A plain (non-TTL) `CloneCached` implementor must provide a required
     implementation of `cache_peek_with_expiry_status`; for stores with no
     expiry the method simply returns `(Some(v), false)` for present keys and
     `(None, false)` for absent keys.

`TtlSortedCache` is covered here too: it implements `CloneCached` and overrides the
method, but is NOT reachable through any `#[cached]` attribute combination, so a
direct store test is the only way to certify its override.
*/

#![cfg(all(feature = "proc_macro", feature = "time_stores"))]

use std::thread::sleep;
use std::time::Duration;

use cached::stores::{
    Expires, ExpiringCache, ExpiringLruCache, LruTtlCache, TtlCache, TtlSortedCache,
};
use cached::{Cached, CloneCached};

// A short-but-nonzero TTL the entry will outlive within a test, then a sleep past
// it to force expiry. Kept small to keep the suite fast while staying deterministic.
const SHORT_TTL: Duration = Duration::from_millis(80);
const PAST_TTL: Duration = Duration::from_millis(160);

// ── Value type for the per-value `Expires` stores ─────────────────────────────
// `is_expired` is driven by a flag baked into the value, so the expiring-store
// tests need no sleep and are fully deterministic.
#[derive(Clone, Debug, PartialEq)]
struct Token {
    payload: i32,
    expired: bool,
}
impl Token {
    fn live(payload: i32) -> Self {
        Token {
            payload,
            expired: false,
        }
    }
    fn stale(payload: i32) -> Self {
        Token {
            payload,
            expired: true,
        }
    }
}
impl Expires for Token {
    fn is_expired(&self) -> bool {
        self.expired
    }
}

// ───────────────────────────────── TtlCache ──────────────────────────────────

#[test]
fn ttl_peek_absent_is_none_false() {
    let c: TtlCache<i32, i32> = TtlCache::builder().ttl(SHORT_TTL).build().unwrap();
    assert_eq!(c.cache_peek_with_expiry_status(&404), (None, false));
}

#[test]
fn ttl_peek_live_is_some_false() {
    let mut c: TtlCache<i32, i32> = TtlCache::builder().ttl(SHORT_TTL).build().unwrap();
    c.cache_set(1, 11);
    assert_eq!(c.cache_peek_with_expiry_status(&1), (Some(11), false));
}

#[test]
fn ttl_peek_expired_surfaces_some_true() {
    // The regression: an expired entry must still be returned (with `true`),
    // not dropped as `None`.
    let mut c: TtlCache<i32, i32> = TtlCache::builder().ttl(SHORT_TTL).build().unwrap();
    c.cache_set(1, 11);
    sleep(PAST_TTL);
    assert_eq!(c.cache_peek_with_expiry_status(&1), (Some(11), true));
}

#[test]
fn ttl_peek_has_no_side_effects() {
    let mut c: TtlCache<i32, i32> = TtlCache::builder().ttl(SHORT_TTL).build().unwrap();
    c.cache_set(1, 11);
    let hits0 = c.cache_hits();
    let misses0 = c.cache_misses();

    // Many peeks on present (live), absent, and expired keys.
    let _ = c.cache_peek_with_expiry_status(&1);
    let _ = c.cache_peek_with_expiry_status(&999);
    sleep(PAST_TTL);
    let _ = c.cache_peek_with_expiry_status(&1);

    assert_eq!(c.cache_hits(), hits0, "peek must not touch hit counter");
    assert_eq!(
        c.cache_misses(),
        misses0,
        "peek must not touch miss counter"
    );
}

#[test]
fn ttl_peek_does_not_renew_ttl_on_refresh_store() {
    // `refresh_on_hit(true)` makes a *real* `cache_get` reset the entry instant.
    // The non-renewing peek must NOT, so the entry stays expired after a peek.
    let mut c: TtlCache<i32, i32> = TtlCache::builder()
        .ttl(SHORT_TTL)
        .refresh_on_hit(true)
        .build()
        .unwrap();
    c.cache_set(1, 11);
    sleep(PAST_TTL);

    // Peek sees it expired and must not renew.
    assert_eq!(c.cache_peek_with_expiry_status(&1), (Some(11), true));
    // A second peek must still report expired: if peek had treated the entry as
    // live and triggered refresh_on_hit, the entry would now be unexpired and
    // the assertion below would fail.
    assert!(
        c.cache_peek_with_expiry_status(&1).1,
        "peek must not renew TTL"
    );
}

// ──────────────────────────────── LruTtlCache ────────────────────────────────

#[test]
fn lru_ttl_peek_absent_live_expired() {
    let mut c: LruTtlCache<i32, i32> = LruTtlCache::builder()
        .max_size(8)
        .ttl(SHORT_TTL)
        .build()
        .unwrap();
    assert_eq!(c.cache_peek_with_expiry_status(&404), (None, false));
    c.cache_set(1, 11);
    assert_eq!(c.cache_peek_with_expiry_status(&1), (Some(11), false));
    sleep(PAST_TTL);
    assert_eq!(c.cache_peek_with_expiry_status(&1), (Some(11), true));
}

#[test]
fn lru_ttl_peek_does_not_promote_recency() {
    // `key_order()` exposes LRU recency. A non-renewing peek of the LRU key must
    // NOT move it to the front; a real `cache_get` would.
    let mut c: LruTtlCache<i32, i32> = LruTtlCache::builder()
        .max_size(8)
        .ttl(SHORT_TTL)
        .build()
        .unwrap();
    c.cache_set(1, 11);
    c.cache_set(2, 22);
    c.cache_set(3, 33);
    let order_before = c.key_order();

    // Peek the least-recently-used key (1). Must not reorder.
    let _ = c.cache_peek_with_expiry_status(&1);
    assert_eq!(
        c.key_order(),
        order_before,
        "peek must not promote LRU recency"
    );

    // Sanity: a real get DOES reorder, proving key_order is recency-sensitive.
    let _ = c.cache_get(&1);
    assert_ne!(
        c.key_order(),
        order_before,
        "control: a real cache_get is expected to change recency order"
    );
}

#[test]
fn lru_ttl_peek_has_no_counter_side_effects() {
    let mut c: LruTtlCache<i32, i32> = LruTtlCache::builder()
        .max_size(8)
        .ttl(SHORT_TTL)
        .build()
        .unwrap();
    c.cache_set(1, 11);
    let hits0 = c.cache_hits();
    let misses0 = c.cache_misses();
    let _ = c.cache_peek_with_expiry_status(&1);
    let _ = c.cache_peek_with_expiry_status(&999);
    assert_eq!(c.cache_hits(), hits0);
    assert_eq!(c.cache_misses(), misses0);
}

// ─────────────────────────────── TtlSortedCache ──────────────────────────────
// Not reachable via `#[cached]`; this is its only override certification.

#[test]
fn ttl_sorted_peek_absent_live_expired() {
    let mut c: TtlSortedCache<i32, i32> = TtlSortedCache::builder().ttl(SHORT_TTL).build().unwrap();
    assert_eq!(c.cache_peek_with_expiry_status(&404), (None, false));
    c.cache_set(1, 11);
    assert_eq!(c.cache_peek_with_expiry_status(&1), (Some(11), false));
    sleep(PAST_TTL);
    assert_eq!(c.cache_peek_with_expiry_status(&1), (Some(11), true));
}

#[test]
fn ttl_sorted_peek_has_no_counter_side_effects() {
    let mut c: TtlSortedCache<i32, i32> = TtlSortedCache::builder().ttl(SHORT_TTL).build().unwrap();
    c.cache_set(1, 11);
    let hits0 = c.cache_hits();
    let misses0 = c.cache_misses();
    let _ = c.cache_peek_with_expiry_status(&1);
    let _ = c.cache_peek_with_expiry_status(&999);
    sleep(PAST_TTL);
    let _ = c.cache_peek_with_expiry_status(&1);
    assert_eq!(c.cache_hits(), hits0);
    assert_eq!(c.cache_misses(), misses0);
}

// ─────────────────────────────── ExpiringCache ───────────────────────────────
// Per-value expiry: deterministic, no sleeps.

#[test]
fn expiring_peek_absent_live_expired() {
    let mut c: ExpiringCache<i32, Token> = ExpiringCache::builder().build().unwrap();
    assert_eq!(c.cache_peek_with_expiry_status(&404), (None, false));
    c.cache_set(1, Token::live(11));
    assert_eq!(
        c.cache_peek_with_expiry_status(&1),
        (Some(Token::live(11)), false)
    );
    // A value that reports itself expired must surface as `(Some, true)`.
    c.cache_set(2, Token::stale(22));
    assert_eq!(
        c.cache_peek_with_expiry_status(&2),
        (Some(Token::stale(22)), true)
    );
}

#[test]
fn expiring_peek_does_not_remove_expired_entry() {
    // `cache_get` removes an expired entry on access; the peek must leave it.
    let mut c: ExpiringCache<i32, Token> = ExpiringCache::builder().build().unwrap();
    c.cache_set(2, Token::stale(22));
    let size_before = c.cache_size();
    let _ = c.cache_peek_with_expiry_status(&2);
    assert_eq!(
        c.cache_size(),
        size_before,
        "peek must not remove the expired entry"
    );
    // Still peekable.
    assert!(c.cache_peek_with_expiry_status(&2).1);
}

#[test]
fn expiring_peek_has_no_counter_side_effects() {
    let mut c: ExpiringCache<i32, Token> = ExpiringCache::builder().build().unwrap();
    c.cache_set(1, Token::live(11));
    c.cache_set(2, Token::stale(22));
    let hits0 = c.cache_hits();
    let misses0 = c.cache_misses();
    let _ = c.cache_peek_with_expiry_status(&1);
    let _ = c.cache_peek_with_expiry_status(&2);
    let _ = c.cache_peek_with_expiry_status(&999);
    assert_eq!(c.cache_hits(), hits0);
    assert_eq!(c.cache_misses(), misses0);
}

// ────────────────────────────── ExpiringLruCache ─────────────────────────────

#[test]
fn expiring_lru_peek_absent_live_expired() {
    let mut c: ExpiringLruCache<i32, Token> =
        ExpiringLruCache::builder().max_size(8).build().unwrap();
    assert_eq!(c.cache_peek_with_expiry_status(&404), (None, false));
    c.cache_set(1, Token::live(11));
    assert_eq!(
        c.cache_peek_with_expiry_status(&1),
        (Some(Token::live(11)), false)
    );
    c.cache_set(2, Token::stale(22));
    assert_eq!(
        c.cache_peek_with_expiry_status(&2),
        (Some(Token::stale(22)), true)
    );
}

#[test]
fn expiring_lru_peek_does_not_promote_recency() {
    // Recency is observable through eviction: with max_size = 2, the LRU key is
    // evicted on overflow. A peek of the LRU key must NOT save it from eviction.
    let mut c: ExpiringLruCache<i32, Token> =
        ExpiringLruCache::builder().max_size(2).build().unwrap();
    c.cache_set(1, Token::live(1)); // LRU
    c.cache_set(2, Token::live(2)); // MRU

    // Peek the LRU key (1). If this promoted recency, key 2 would become LRU and
    // be evicted instead of key 1 on the next insert.
    let _ = c.cache_peek_with_expiry_status(&1);

    c.cache_set(3, Token::live(3)); // overflow -> evict the still-LRU key 1
    assert_eq!(
        c.cache_get(&1),
        None,
        "key 1 must still be LRU and get evicted (peek must not promote it)"
    );
    assert_eq!(c.cache_get(&2), Some(&Token::live(2)));
    assert_eq!(c.cache_get(&3), Some(&Token::live(3)));
}

#[test]
fn expiring_lru_peek_has_no_counter_side_effects() {
    let mut c: ExpiringLruCache<i32, Token> =
        ExpiringLruCache::builder().max_size(8).build().unwrap();
    c.cache_set(1, Token::live(11));
    c.cache_set(2, Token::stale(22));
    let hits0 = c.cache_hits();
    let misses0 = c.cache_misses();
    let _ = c.cache_peek_with_expiry_status(&1);
    let _ = c.cache_peek_with_expiry_status(&2);
    let _ = c.cache_peek_with_expiry_status(&999);
    assert_eq!(c.cache_hits(), hits0);
    assert_eq!(c.cache_misses(), misses0);
}

// ────────────────── required method on a plain non-TTL store (#3) ─────────────
//
// `cache_peek_with_expiry_status` is now a required method. A plain (non-TTL)
// external `CloneCached` implementor with no expiry should implement it to return
// `(Some(v), false)` for present keys and `(None, false)` for absent keys -- the
// same side-effect-free shape as the built-in stores, just with entries that are
// never expired.

#[derive(Default)]
struct PlainPeekStore {
    map: std::collections::HashMap<i32, i32>,
}

impl Cached<i32, i32> for PlainPeekStore {
    type Error = std::convert::Infallible;

    fn cache_get<Q>(&mut self, k: &Q) -> Option<&i32>
    where
        i32: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.map.get(k)
    }
    fn cache_get_mut<Q>(&mut self, k: &Q) -> Option<&mut i32>
    where
        i32: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.map.get_mut(k)
    }
    fn cache_set(&mut self, k: i32, v: i32) -> Option<i32> {
        self.map.insert(k, v)
    }
    fn cache_get_or_set_with_mut<F: FnOnce() -> i32>(&mut self, k: i32, f: F) -> &mut i32 {
        self.map.entry(k).or_insert_with(f)
    }
    fn cache_try_get_or_set_with_mut<F: FnOnce() -> Result<i32, E>, E>(
        &mut self,
        k: i32,
        f: F,
    ) -> Result<&mut i32, E> {
        use std::collections::hash_map::Entry;
        let v = match self.map.entry(k) {
            Entry::Occupied(o) => o.into_mut(),
            Entry::Vacant(v) => v.insert(f()?),
        };
        Ok(v)
    }
    fn cache_remove<Q>(&mut self, k: &Q) -> Option<i32>
    where
        i32: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.map.remove(k)
    }
    fn cache_remove_entry<Q>(&mut self, k: &Q) -> Option<(i32, i32)>
    where
        i32: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.map.remove_entry(k)
    }
    fn cache_clear(&mut self) {
        self.map.clear();
    }
    fn cache_reset(&mut self) {
        self.map.clear();
    }
    fn cache_size(&self) -> usize {
        self.map.len()
    }
}

impl CloneCached<i32, i32> for PlainPeekStore {
    fn cache_get_with_expiry_status<Q>(&mut self, k: &Q) -> (Option<i32>, bool)
    where
        i32: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        (self.map.get(k).copied(), false)
    }

    // Required: side-effect-free read; plain store has no expiry so never returns true.
    fn cache_peek_with_expiry_status<Q>(&self, k: &Q) -> (Option<i32>, bool)
    where
        i32: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
        i32: Clone,
    {
        (self.map.get(k).copied(), false)
    }
}

#[test]
fn required_cache_peek_with_expiry_status_on_plain_store() {
    let mut store = PlainPeekStore::default();
    store.cache_set(1, 11);

    // Present key: returns (Some(v), false) -- plain store, entries never expire.
    assert_eq!(store.cache_peek_with_expiry_status(&1), (Some(11), false));
    // Absent key: returns (None, false).
    assert_eq!(store.cache_peek_with_expiry_status(&999), (None, false));
    // The renewing read agrees on the same value.
    assert_eq!(store.cache_get_with_expiry_status(&1), (Some(11), false));
}
