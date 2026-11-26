// Pure-function coverage for the Redis millisecond TTL helpers: reject zero,
// preserve sub-second precision, clamp to `i64::MAX` ms. These need no
// Redis server and guard both the `PSETEX` (cache_set) and `PEXPIRE`
// (refresh) paths, which share `ttl_millis`/`ttl_millis_i64`.
use super::{ttl_millis, ttl_millis_i64};
use crate::time::Duration;

#[test]
fn zero_is_rejected() {
    assert!(ttl_millis(Duration::ZERO).is_err());
    assert!(ttl_millis_i64(Duration::ZERO).is_err());
}

#[test]
fn whole_seconds_become_milliseconds() {
    assert_eq!(ttl_millis(Duration::from_secs(1)).unwrap(), 1_000);
    assert_eq!(ttl_millis(Duration::from_secs(60)).unwrap(), 60_000);
    assert_eq!(ttl_millis_i64(Duration::from_secs(60)).unwrap(), 60_000);
}

#[test]
fn subsecond_precision_is_preserved() {
    // Unlike the old SETEX path, sub-second TTLs are NOT rounded up to 1s.
    assert_eq!(ttl_millis(Duration::from_millis(1)).unwrap(), 1);
    assert_eq!(ttl_millis(Duration::from_millis(250)).unwrap(), 250);
    assert_eq!(ttl_millis(Duration::from_millis(999)).unwrap(), 999);
}

#[test]
fn nonzero_submillisecond_clamps_to_one() {
    // A non-zero Duration under 1ms truncates to 0ms, which PSETEX/PEXPIRE
    // reject (or treat as immediate-delete). The low-end clamp lifts it to 1ms.
    let one_ns = Duration::from_nanos(1);
    assert!(!one_ns.is_zero());
    // Truncation to milliseconds is still 0...
    assert_eq!(one_ns.as_millis(), 0);
    // ...but ttl_millis clamps a non-zero sub-ms Duration up to 1ms.
    assert_eq!(ttl_millis(one_ns).unwrap(), 1);
    assert_eq!(ttl_millis_i64(one_ns).unwrap(), 1);
    // Other sub-millisecond, non-zero durations also clamp to 1.
    assert_eq!(ttl_millis(Duration::from_nanos(999_999)).unwrap(), 1);
    assert_eq!(ttl_millis(Duration::from_micros(500)).unwrap(), 1);
}

#[test]
fn zero_never_reaches_the_clamp() {
    // The is_zero guard rejects an actually-zero Duration before the
    // low-end `.max(1)` clamp can lift it to 1ms.
    assert!(ttl_millis(Duration::ZERO).is_err());
    assert!(ttl_millis_i64(Duration::ZERO).is_err());
}

#[test]
fn subsecond_mixed_passes_through() {
    assert_eq!(ttl_millis(Duration::from_millis(1_500)).unwrap(), 1_500);
    assert_eq!(ttl_millis(Duration::new(5, 1_000_000)).unwrap(), 5_001);
}

#[test]
fn very_large_clamps_to_i64_max() {
    // A Duration that, in milliseconds, overflows i64 is clamped.
    let huge = Duration::from_secs(u64::MAX);
    assert_eq!(ttl_millis(huge).unwrap(), i64::MAX as u64);
    assert_eq!(ttl_millis_i64(huge).unwrap(), i64::MAX);
}
