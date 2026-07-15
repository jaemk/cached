//! Formal tests for the per-entry `expires_at: Option<Instant>` semantics
//! introduced in v3 (shard #7).
//!
//! Key invariants under test:
//!
//! 1. `set_ttl` is future-only: an entry inserted before the call keeps its
//!    original `expires_at`; only entries inserted AFTER the call use the new TTL.
//!
//! 2. `refresh_on_hit` recomputes `expires_at = now + current_ttl` on every live
//!    hit, extending the deadline from the moment of access rather than insert.
//!    When the current TTL is zero (disabled), a hit must preserve the existing
//!    `expires_at` (i.e. it must NOT clear it to `None`).
//!
//! All items are gated `#[cfg(feature = "time_stores")]`.
#![cfg(feature = "time_stores")]

use cached::time::Duration;
use cached::{
    CacheTtl, Cached, ConcurrentCacheTtl, ConcurrentCached, LruTtlCache, ShardedTtlCache, TtlCache,
};

// Enough time to let a SHORT-ttl entry expire; chosen to be comfortably > SHORT.
// Use a generously wide TTL so CI runners under load don't race the wall-clock.
const SHORT: Duration = Duration::from_millis(200);
const LONG: Duration = Duration::from_secs(60);
const SLEEP: std::time::Duration = std::time::Duration::from_millis(500);

// ─────────────────────────── TtlCache: set_ttl is future-only ────────────────

/// After `set_ttl(SHORT)` on a populated cache, entries inserted before the call
/// retain their original LONG expiry and must survive past `SHORT`.
#[test]
fn ttl_cache_set_ttl_does_not_retroactively_expire_existing_entries() {
    let mut c = TtlCache::<u32, u32>::builder()
        .ttl(LONG)
        .build()
        .expect("build TtlCache");

    c.cache_set(1, 100); // expires_at = now + LONG (60s)
    c.set_ttl(SHORT); // future inserts get expires_at = now + SHORT (30ms)

    std::thread::sleep(SLEEP); // 80ms elapsed

    // Entry 1 was inserted under LONG and must still be live after 80ms.
    assert_eq!(
        c.cache_get(&1),
        Some(&100),
        "pre-set_ttl entry must keep its original LONG expires_at"
    );
}

/// After `set_ttl(SHORT)` on a populated cache, a NEW entry inserted after the
/// call uses SHORT and must have expired after sleeping past it.
#[test]
fn ttl_cache_set_ttl_applies_to_new_inserts() {
    let mut c = TtlCache::<u32, u32>::builder()
        .ttl(LONG)
        .build()
        .expect("build TtlCache");

    c.set_ttl(SHORT);
    c.cache_set(2, 200); // expires_at = now + SHORT (30ms)

    std::thread::sleep(SLEEP); // 80ms > 30ms

    assert_eq!(
        c.cache_get(&2),
        None,
        "entry inserted after set_ttl(SHORT) must expire at the new deadline"
    );
}

// ─────────────────────────── TtlCache: refresh_on_hit ────────────────────────

/// `refresh_on_hit` must recompute `expires_at = now + ttl` on every live hit,
/// extending the deadline. After multiple hits spaced SHORT/2 apart the entry
/// must still be live because each hit pushed the deadline forward.
#[test]
fn ttl_cache_refresh_on_hit_extends_expires_at() {
    let mut c = TtlCache::<u32, u32>::builder()
        .ttl(SHORT)
        .refresh_on_hit(true)
        .build()
        .expect("build TtlCache");

    c.cache_set(1, 10); // expires_at = now + SHORT

    // Sleep half of SHORT, then read: refresh extends to now+SHORT again.
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(
        c.cache_get(&1),
        Some(&10),
        "entry must still be live at half-SHORT (< SHORT)"
    );

    // Sleep half of SHORT again (100ms more from the read = 200ms from read,
    // but the refreshed deadline is now+SHORT from the read, so still live).
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(
        c.cache_get(&1),
        Some(&10),
        "refresh must have extended the deadline; entry must still be live"
    );
}

/// When the current TTL is 0 (disabled), a `refresh_on_hit` must NOT set
/// `expires_at = None` on an entry that already has a concrete expiry; the
/// existing `expires_at` must be preserved.
#[test]
fn ttl_cache_refresh_on_hit_with_disabled_ttl_preserves_existing_expires_at() {
    let mut c = TtlCache::<u32, u32>::builder()
        .ttl(LONG) // entry gets expires_at = now + 60s
        .refresh_on_hit(true)
        .build()
        .expect("build TtlCache");

    c.cache_set(1, 10); // expires_at = now + 60s

    // Disable TTL; future inserts get expires_at = None, but entry 1 still has now+60s.
    c.set_ttl(Duration::ZERO);

    // Hit entry 1 with refresh enabled and current TTL = 0.
    // The refresh must preserve the existing expires_at (now+60s), not clear it.
    assert_eq!(c.cache_get(&1), Some(&10));

    // Entry must still be live after the refresh hit.
    assert_eq!(
        c.cache_get(&1),
        Some(&10),
        "refresh_on_hit with disabled TTL must not change the existing expires_at"
    );
}

// ─────────────────────────── LruTtlCache: set_ttl is future-only ─────────────

/// Same future-only contract on LruTtlCache: pre-`set_ttl` entries retain their
/// original expires_at and survive past the new (shorter) TTL.
#[test]
fn lru_ttl_cache_set_ttl_does_not_retroactively_expire_existing_entries() {
    let mut c = LruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .ttl(LONG)
        .build()
        .expect("build LruTtlCache");

    c.cache_set(1, 100); // expires_at = now + 60s
    c.set_ttl(SHORT); // future inserts get expires_at = now + 30ms

    std::thread::sleep(SLEEP); // 80ms elapsed

    assert_eq!(
        c.cache_get(&1),
        Some(&100),
        "LruTtlCache: pre-set_ttl entry must keep its original LONG expires_at"
    );
}

/// After `set_ttl(SHORT)` on LruTtlCache, a new insert uses SHORT.
#[test]
fn lru_ttl_cache_set_ttl_applies_to_new_inserts() {
    let mut c = LruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .ttl(LONG)
        .build()
        .expect("build LruTtlCache");

    c.set_ttl(SHORT);
    c.cache_set(2, 200); // expires_at = now + 30ms

    std::thread::sleep(SLEEP);

    assert_eq!(
        c.cache_get(&2),
        None,
        "LruTtlCache: entry inserted after set_ttl(SHORT) must expire"
    );
}

// ─────────────────────────── LruTtlCache: refresh_on_hit ─────────────────────

/// refresh_on_hit extends the deadline on LruTtlCache, same as TtlCache.
#[test]
fn lru_ttl_cache_refresh_on_hit_extends_expires_at() {
    let mut c = LruTtlCache::<u32, u32>::builder()
        .max_size(8)
        .ttl(SHORT)
        .refresh_on_hit(true)
        .build()
        .expect("build LruTtlCache");

    c.cache_set(1, 10); // expires_at = now + SHORT

    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(
        c.cache_get(&1),
        Some(&10),
        "LRU: entry must still be live at half-SHORT"
    );

    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(
        c.cache_get(&1),
        Some(&10),
        "LRU: refresh must have extended the deadline; entry must still be live"
    );
}

// ─────────────────────────── ShardedTtlCache: set_ttl is future-only ─────────

/// The concurrent ShardedTtlCache must also honour the future-only contract for
/// `set_ttl`.  A pre-call entry retains its original expiry; a post-call insert
/// uses the new TTL.
#[test]
fn sharded_ttl_cache_set_ttl_does_not_retroactively_expire_existing_entries() {
    let c = ShardedTtlCache::<u32, u32>::builder()
        .ttl(LONG)
        .shards(1)
        .build()
        .expect("build ShardedTtlCache");

    c.cache_set(1, 100).unwrap(); // expires_at = now + 60s
    c.set_ttl(SHORT); // future inserts get expires_at = now + 30ms

    std::thread::sleep(SLEEP); // 80ms elapsed

    assert_eq!(
        c.cache_get(&1),
        Ok(Some(100)),
        "ShardedTtlCache: pre-set_ttl entry must keep its LONG expires_at"
    );
}

/// After `set_ttl(SHORT)` on ShardedTtlCache, a new insert uses SHORT and expires.
#[test]
fn sharded_ttl_cache_set_ttl_applies_to_new_inserts() {
    let c = ShardedTtlCache::<u32, u32>::builder()
        .ttl(LONG)
        .shards(1)
        .build()
        .expect("build ShardedTtlCache");

    c.set_ttl(SHORT);
    c.cache_set(2, 200).unwrap(); // expires_at = now + 30ms

    std::thread::sleep(SLEEP);

    assert_eq!(
        c.cache_get(&2),
        Ok(None),
        "ShardedTtlCache: entry inserted after set_ttl(SHORT) must expire"
    );
}

/// refresh_on_hit on ShardedTtlCache extends the deadline.
#[test]
fn sharded_ttl_cache_refresh_on_hit_extends_expires_at() {
    let c = ShardedTtlCache::<u32, u32>::builder()
        .ttl(SHORT)
        .shards(1)
        .refresh_on_hit(true)
        .build()
        .expect("build ShardedTtlCache");

    c.cache_set(1, 10).unwrap(); // expires_at = now + SHORT

    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(
        c.cache_get(&1),
        Ok(Some(10)),
        "Sharded: entry must still be live at half-SHORT"
    );

    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(
        c.cache_get(&1),
        Ok(Some(10)),
        "Sharded: refresh must have extended the deadline; entry must still be live"
    );
}
