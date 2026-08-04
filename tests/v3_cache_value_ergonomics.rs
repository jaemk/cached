//! Consumer-experience regression tests for `CacheValue<V, M>` ergonomics gaps found in
//! external review: `Display` did not forward to the inner value, there was no bulk-unwrap
//! helper for `value_order()`/`iter_order()` results, and the deliberate asymmetry of the
//! `PartialEq<V>` impl (`wrapped == bare` compiles, `bare == wrapped` does not) needed a
//! regression guard so it is not silently dropped.
//!
//! `LruCache` (`M = ()`) is always available; `LruTtlCache` (`M = Option<Instant>`) lives
//! behind `time_stores` (default-on), so its cases are gated accordingly.

use cached::stores::{CacheValue, IntoValues};
use cached::{Cached, LruCache};

/// `Display` on `CacheValue<V, ()>` (the `LruCache` / `ExpiringLruCache` metadata shape)
/// renders exactly the inner value's `Display` output.
///
/// `CacheValue::new` is crate-private, so the wrapper is obtained the way a real consumer
/// would: via a store's `value_order()`/`iter_order()`.
///
/// Fails to compile without `impl<V: Display, M> Display for CacheValue<V, M>`.
#[test]
fn display_forwards_to_inner_value_unit_meta() {
    let mut c: LruCache<u32, u32> = LruCache::new(10);
    c.cache_set(1, 42);
    let cv: CacheValue<u32> = c.value_order().into_iter().next().unwrap();
    assert_eq!(format!("{cv}"), "42");
    assert_eq!(cv.to_string(), 42u32.to_string());

    let mut c_str: LruCache<u32, String> = LruCache::new(10);
    c_str.cache_set(1, "hello".to_string());
    let cv_str: CacheValue<String> = c_str.value_order().into_iter().next().unwrap();
    assert_eq!(format!("{cv_str}"), "hello");
}

/// Same as above, but for the `M = Option<Instant>` metadata carried by `LruTtlCache`,
/// confirming `Display` forwards regardless of which metadata type is present.
#[cfg(feature = "time_stores")]
#[test]
fn display_forwards_to_inner_value_instant_meta() {
    use cached::LruTtlCache;
    use cached::time::{Duration, Instant};
    use cached::{CacheTtl, Cached};

    // With a live TTL, metadata is `Some(instant)`.
    let mut c: LruTtlCache<u32, u32> = LruTtlCache::new(10, Duration::from_secs(3600));
    c.cache_set(1, 7);
    let cv: CacheValue<u32, Option<Instant>> = c.value_order().into_iter().next().unwrap();
    assert_eq!(format!("{cv}"), "7");
    assert!(cv.expires_at().is_some());

    // With the TTL unset, newly inserted entries carry `None` metadata; `Display` still
    // forwards to the inner value regardless of which metadata variant is present.
    c.unset_ttl();
    c.cache_set(2, 9);
    let cv_none: CacheValue<u32, Option<Instant>> = c
        .value_order()
        .into_iter()
        .find(|cv| **cv == 9)
        .expect("entry for key 2");
    assert_eq!(format!("{cv_none}"), "9");
    assert!(cv_none.expires_at().is_none());
}

/// `println!("{v}")`-style interpolation (the exact pattern from the review report) compiles
/// and renders the inner value for a `value_order()` result.
#[test]
fn display_works_in_format_args_on_value_order_result() {
    let mut c: LruCache<u32, u32> = LruCache::new(10);
    c.cache_set(1, 100);
    let vals = c.value_order();
    let v = &vals[0];
    // This is the exact call shape from the review report: `println!("{k} -> {v}")`.
    let rendered = format!("{v}");
    assert_eq!(rendered, "100");
}

/// The bulk-unwrap helper turns a `Vec<CacheValue<V, M>>` (the `value_order()` shape) into a
/// `Vec<V>`, preserving order.
///
/// Fails to compile without the `IntoValues` extension trait (or an equivalent free function).
#[test]
fn into_values_unwraps_value_order_vec_preserving_order() {
    let mut c: LruCache<u32, u32> = LruCache::new(10);
    c.cache_set(1, 100);
    c.cache_set(2, 200);
    c.cache_set(3, 300);

    // Recency order: most-recently-used first per LruCache::value_order convention.
    let wrapped = c.value_order();
    let wrapped_plain: Vec<u32> = wrapped.iter().map(|cv| **cv).collect();

    let unwrapped: Vec<u32> = c.value_order().into_values();
    assert_eq!(unwrapped, wrapped_plain);
    // Concretely pin down the order too, so this isn't vacuously true if both sides were empty.
    assert_eq!(unwrapped, vec![300, 200, 100]);
}

/// The bulk-unwrap helper also handles the `iter_order()` shape (`Vec<(K, CacheValue<V, M>)>`),
/// discarding keys and preserving value order.
#[test]
fn into_values_unwraps_iter_order_vec_preserving_order() {
    let mut c: LruCache<u32, u32> = LruCache::new(10);
    c.cache_set(1, 100);
    c.cache_set(2, 200);
    c.cache_set(3, 300);

    let entries = c.iter_order();
    let expected: Vec<u32> = entries.iter().map(|(_, cv)| **cv).collect();

    let unwrapped: Vec<u32> = c.iter_order().into_values();
    assert_eq!(unwrapped, expected);
    assert_eq!(unwrapped, vec![300, 200, 100]);
}

/// `into_values()` on an empty collection returns an empty `Vec`, not a panic.
#[test]
fn into_values_empty_collection() {
    let c: LruCache<u32, u32> = LruCache::new(10);
    let unwrapped: Vec<u32> = c.value_order().into_values();
    assert!(unwrapped.is_empty());

    let unwrapped_entries: Vec<u32> = c.iter_order().into_values();
    assert!(unwrapped_entries.is_empty());
}

/// Regression guard on the existing `PartialEq<V>` impl: `wrapped == bare` must still hold.
/// (The reverse, `bare == wrapped`, is a deliberate coherence-forbidden limitation and is
/// intentionally NOT tested here -- it does not compile.)
#[test]
fn wrapped_equals_bare_value() {
    let mut c: LruCache<u32, u32> = LruCache::new(10);
    c.cache_set(1, 2);
    let cv: CacheValue<u32> = c.value_order().into_iter().next().unwrap();
    assert_eq!(cv, 2u32);
    assert!(cv == 2u32);
    assert!(!(cv == 3u32));
}
