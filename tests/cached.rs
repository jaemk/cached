/*!
Full tests of macro-defined functions
*/

#[cfg(feature = "time_stores")]
use cached::time::Duration;
#[cfg(feature = "proc_macro")]
use cached::{macros::cached, macros::once};
use cached::{Cached, LruCache, UnboundCache};
use cached::{Expires, ExpiringLruCache};
#[cfg(all(not(feature = "time_stores"), feature = "proc_macro"))]
use std::time::Duration;

#[test]
#[cfg(feature = "proc_macro")]
fn compile_fail_unsync_reads_basic() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/unsync_reads_sized_cache.rs");
    t.compile_fail("tests/ui/unsync_reads_mutex_lock.rs");
    t.compile_fail("tests/ui/result_fallback_without_result.rs");
    t.compile_fail("tests/ui/with_cached_flag_return_like.rs");
    t.compile_fail("tests/ui/with_cached_flag_foreign_return.rs");
    t.compile_fail("tests/ui/cached_with_cached_flag_unqualified_return.rs");
    t.compile_fail("tests/ui/once_by_key_rejected.rs");
    t.compile_fail("tests/ui/time_attr_renamed.rs");
    t.compile_fail("tests/ui/time_refresh_attr_renamed.rs");
    t.compile_fail("tests/ui/result_fallback_unbound_cache.rs");
    t.compile_fail("tests/ui/concurrent_cached_time_refresh_attr_renamed.rs");
}

#[test]
#[cfg(all(feature = "proc_macro", feature = "time_stores"))]
fn compile_fail_unsync_reads_timed() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/unsync_reads_timed_cache.rs");
}

// One negative trybuild case per *semantic* compile error the macros raise
// (i.e. errors we define for invalid attribute/signature states). Pure
// syn-parser pass-through messages for malformed user strings (bad `ty` /
// `create` / `convert` / `map_error`) are intentionally excluded: their
// rendered output is syn-version-dependent and would make brittle goldens.
// Every error here fires during macro expansion before any feature-gated
// store type is emitted, so `proc_macro` alone is sufficient.
#[test]
#[cfg(feature = "proc_macro")]
fn compile_fail_macro_arg_validation() {
    let t = trybuild::TestCases::new();

    // ---- #[cached] ----
    t.compile_fail("tests/ui/cached_self_method.rs");
    t.compile_fail("tests/ui/cached_with_cached_flag_no_return.rs");
    t.compile_fail("tests/ui/cached_result_option_exclusive.rs");
    t.compile_fail("tests/ui/cached_result_no_return.rs");
    t.compile_fail("tests/ui/cached_result_no_inner_type.rs");
    t.compile_fail("tests/ui/cached_result_complex_return.rs");
    t.compile_fail("tests/ui/cached_key_without_convert.rs");
    t.compile_fail("tests/ui/cached_convert_without_key.rs");
    t.compile_fail("tests/ui/cached_ty_without_create.rs");
    t.compile_fail("tests/ui/cached_create_without_ty.rs");
    t.compile_fail("tests/ui/cached_store_types_exclusive.rs");
    t.compile_fail("tests/ui/cached_sync_writes_buckets_zero.rs");
    t.compile_fail("tests/ui/cached_result_fallback_sync_writes.rs");
    t.compile_fail("tests/ui/cached_sync_lock_unknown.rs");

    // ---- #[once] ----
    t.compile_fail("tests/ui/once_self_method.rs");
    t.compile_fail("tests/ui/once_time_attr_renamed.rs");
    t.compile_fail("tests/ui/once_with_cached_flag_foreign.rs");
    t.compile_fail("tests/ui/once_result_option_exclusive.rs");
    t.compile_fail("tests/ui/once_result_no_return.rs");
    t.compile_fail("tests/ui/once_sync_writes_buckets_zero.rs");

    // ---- #[concurrent_cached] ----
    t.compile_fail("tests/ui/concurrent_cached_self_method.rs");
    t.compile_fail("tests/ui/concurrent_cached_time_attr_renamed.rs");
    t.compile_fail("tests/ui/concurrent_cached_with_cached_flag_foreign.rs");
    t.compile_fail("tests/ui/concurrent_cached_no_return.rs");
    t.compile_fail("tests/ui/concurrent_cached_complex_return.rs");
    t.compile_fail("tests/ui/concurrent_cached_non_result_return.rs");
    t.compile_fail("tests/ui/concurrent_cached_redis_create_conflict.rs");
    t.compile_fail("tests/ui/concurrent_cached_async_redis_no_ttl.rs");
    t.compile_fail("tests/ui/concurrent_cached_redis_no_ttl.rs");
    t.compile_fail("tests/ui/concurrent_cached_disk_create_conflict.rs");
    t.compile_fail("tests/ui/concurrent_cached_disk_create_ignored_attrs.rs");
    t.compile_fail("tests/ui/concurrent_cached_option_return.rs");
    t.compile_fail("tests/ui/concurrent_cached_custom_ty_required.rs");
    t.compile_fail("tests/ui/concurrent_cached_custom_create_required.rs");
    t.compile_fail("tests/ui/concurrent_cached_key_without_convert.rs");
}

#[cfg(feature = "proc_macro")]
use serial_test::serial;
#[cfg(any(feature = "proc_macro", feature = "time_stores"))]
use std::thread::sleep;

// NoClone is not cloneable. So this also tests that the Result type
// itself does not have to be cloneable, just the type for the Ok
// value.
// Vec has Clone, but not Copy, to make sure Copy isn't required.
#[cfg(feature = "proc_macro")]
struct NoClone {}

#[cfg(feature = "proc_macro")]
#[cached(unsync_reads = true)]
fn unsync_double(n: u32) -> u32 {
    n * 2
}

#[cfg(feature = "proc_macro")]
#[cached(unsync_reads = true, sync_writes = "default")]
fn unsync_double_sync_writes(n: u32) -> u32 {
    n * 2
}

#[cfg(feature = "proc_macro")]
#[cached(result = true)]
fn proc_cached_result(n: u32) -> Result<Vec<u32>, NoClone> {
    if n < 5 {
        Ok(vec![n])
    } else {
        Err(NoClone {})
    }
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_proc_cached_result() {
    assert!(proc_cached_result(2).is_ok());
    assert!(proc_cached_result(4).is_ok());
    assert!(proc_cached_result(6).is_err());
    assert!(proc_cached_result(6).is_err());
    assert!(proc_cached_result(2).is_ok());
    assert!(proc_cached_result(4).is_ok());
    {
        let cache = PROC_CACHED_RESULT.read();
        assert_eq!(2, cache.cache_size());
        assert_eq!(2, cache.cache_hits().unwrap());
        assert_eq!(4, cache.cache_misses().unwrap());
    }
}

#[cfg(feature = "proc_macro")]
#[cached(option = true)]
fn proc_cached_option(n: u32) -> Option<Vec<u32>> {
    if n < 5 {
        Some(vec![n])
    } else {
        None
    }
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_proc_cached_option() {
    assert!(proc_cached_option(2).is_some());
    assert!(proc_cached_option(4).is_some());
    assert!(proc_cached_option(1).is_some());
    assert!(proc_cached_option(6).is_none());
    assert!(proc_cached_option(6).is_none());
    assert!(proc_cached_option(2).is_some());
    assert!(proc_cached_option(1).is_some());
    assert!(proc_cached_option(4).is_some());
    {
        let cache = PROC_CACHED_OPTION.read();
        assert_eq!(3, cache.cache_size());
        assert_eq!(3, cache.cache_hits().unwrap());
        assert_eq!(5, cache.cache_misses().unwrap());
    }
}

#[cfg(feature = "proc_macro")]
#[cached(with_cached_flag = true)]
fn cached_return_flag(n: i32) -> cached::Return<i32> {
    cached::Return::new(n)
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_cached_return_flag() {
    let r = cached_return_flag(1);
    assert!(!r.was_cached);
    assert_eq!(*r, 1);
    let r = cached_return_flag(1);
    assert!(r.was_cached);
    // derefs to inner
    assert_eq!(*r, 1);
    assert!(r.is_positive());
    {
        let cache = CACHED_RETURN_FLAG.read();
        assert_eq!(cache.cache_hits(), Some(1));
        assert_eq!(cache.cache_misses(), Some(1));
    }
}

#[cfg(feature = "proc_macro")]
#[cached(result = true, with_cached_flag = true)]
fn cached_return_flag_result(n: i32) -> Result<cached::Return<i32>, ()> {
    if n == 10 {
        return Err(());
    }
    Ok(cached::Return::new(n))
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_cached_return_flag_result() {
    let r = cached_return_flag_result(1).unwrap();
    assert!(!r.was_cached);
    assert_eq!(*r, 1);
    let r = cached_return_flag_result(1).unwrap();
    assert!(r.was_cached);
    // derefs to inner
    assert_eq!(*r, 1);
    assert!(r.is_positive());

    let r = cached_return_flag_result(10);
    assert!(r.is_err());
    {
        let cache = CACHED_RETURN_FLAG_RESULT.read();
        assert_eq!(cache.cache_hits(), Some(1));
        assert_eq!(cache.cache_misses(), Some(2));
    }
}

#[cfg(feature = "proc_macro")]
#[cached(option = true, with_cached_flag = true)]
fn cached_return_flag_option(n: i32) -> Option<cached::Return<i32>> {
    if n == 10 {
        return None;
    }
    Some(cached::Return::new(n))
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_cached_return_flag_option() {
    let r = cached_return_flag_option(1).unwrap();
    assert!(!r.was_cached);
    assert_eq!(*r, 1);
    let r = cached_return_flag_option(1).unwrap();
    assert!(r.was_cached);
    // derefs to inner
    assert_eq!(*r, 1);
    assert!(r.is_positive());

    let r = cached_return_flag_option(10);
    assert!(r.is_none());
    {
        let cache = CACHED_RETURN_FLAG_OPTION.read();
        assert_eq!(cache.cache_hits(), Some(1));
        assert_eq!(cache.cache_misses(), Some(2));
    }
}

/// should only cache the _first_ `Ok` returned.
/// all arguments are ignored for subsequent calls.
#[cfg(feature = "proc_macro")]
#[once(result = true)]
fn only_cached_result_once(s: String, error: bool) -> std::result::Result<Vec<String>, u32> {
    if error {
        Err(1)
    } else {
        Ok(vec![s])
    }
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_only_cached_result_once() {
    assert!(only_cached_result_once("z".to_string(), true).is_err());
    let a = only_cached_result_once("a".to_string(), false).unwrap();
    let b = only_cached_result_once("b".to_string(), false).unwrap();
    assert_eq!(a, b);
    sleep(Duration::new(1, 0));
    let b = only_cached_result_once("b".to_string(), false).unwrap();
    assert_eq!(a, b);
}

/// should only cache the _first_ `Some` returned .
/// all arguments are ignored for subsequent calls
#[cfg(feature = "proc_macro")]
#[once(option = true)]
fn only_cached_option_once(s: String, none: bool) -> Option<Vec<String>> {
    if none {
        None
    } else {
        Some(vec![s])
    }
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_only_cached_option_once() {
    assert!(only_cached_option_once("z".to_string(), true).is_none());
    let a = only_cached_option_once("a".to_string(), false).unwrap();
    let b = only_cached_option_once("b".to_string(), false).unwrap();
    assert_eq!(a, b);
    sleep(Duration::new(1, 0));
    let b = only_cached_option_once("b".to_string(), false).unwrap();
    assert_eq!(a, b);
}

#[cfg(feature = "proc_macro")]
#[cached(size = 2)]
fn cached_smartstring(s: smartstring::alias::String) -> smartstring::alias::String {
    if s == "very stringy" {
        smartstring::alias::String::from("equal")
    } else {
        smartstring::alias::String::from("not equal")
    }
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_cached_smartstring() {
    let mut string = smartstring::alias::String::new();
    string.push_str("very stringy");
    assert_eq!("equal", cached_smartstring(string.clone()));
    {
        let cache = CACHED_SMARTSTRING.read();
        assert_eq!(cache.cache_hits(), Some(0));
        assert_eq!(cache.cache_misses(), Some(1));
    }

    assert_eq!("equal", cached_smartstring(string.clone()));
    {
        let cache = CACHED_SMARTSTRING.read();
        assert_eq!(cache.cache_hits(), Some(1));
        assert_eq!(cache.cache_misses(), Some(1));
    }

    let string = smartstring::alias::String::from("also stringy");
    assert_eq!("not equal", cached_smartstring(string));
    {
        let cache = CACHED_SMARTSTRING.read();
        assert_eq!(cache.cache_hits(), Some(1));
        assert_eq!(cache.cache_misses(), Some(2));
    }
}

#[cfg(feature = "proc_macro")]
#[cached(
    size = 2,
    key = "smartstring::alias::String",
    convert = r#"{ smartstring::alias::String::from(s) }"#
)]
fn cached_smartstring_from_str(s: &str) -> bool {
    s == "true"
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_cached_smartstring_from_str() {
    assert!(cached_smartstring_from_str("true"));
    {
        let cache = CACHED_SMARTSTRING_FROM_STR.read();
        assert_eq!(cache.cache_hits(), Some(0));
        assert_eq!(cache.cache_misses(), Some(1));
    }

    assert!(cached_smartstring_from_str("true"));
    {
        let cache = CACHED_SMARTSTRING_FROM_STR.read();
        assert_eq!(cache.cache_hits(), Some(1));
        assert_eq!(cache.cache_misses(), Some(1));
    }

    assert!(!cached_smartstring_from_str("false"));
    {
        let cache = CACHED_SMARTSTRING_FROM_STR.read();
        assert_eq!(cache.cache_hits(), Some(1));
        assert_eq!(cache.cache_misses(), Some(2));
    }
}

#[cfg(feature = "proc_macro")]
#[once]
fn once_for_priming() -> bool {
    true
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_once_for_priming() {
    assert!(once_for_priming_prime_cache());
    {
        let cache = ONCE_FOR_PRIMING.read();
        assert!(cache.is_some());
    }
}

#[cfg(feature = "proc_macro")]
#[cached]
fn mutable_args(mut a: i32, mut b: i32) -> (i32, i32) {
    a += 1;
    b += 1;
    (a, b)
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_mutable_args() {
    assert_eq!((2, 2), mutable_args(1, 1));
    assert_eq!((2, 2), mutable_args(1, 1));
}

#[cfg(feature = "proc_macro")]
#[cached]
fn mutable_args_str(mut a: String) -> String {
    a.push_str("-ok");
    a
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_mutable_args_str() {
    assert_eq!("a-ok", mutable_args_str(String::from("a")));
    assert_eq!("a-ok", mutable_args_str(String::from("a")));
}

#[cfg(feature = "proc_macro")]
#[once]
fn mutable_args_once(mut a: i32, mut b: i32) -> (i32, i32) {
    a += 1;
    b += 1;
    (a, b)
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_mutable_args_once() {
    assert_eq!((2, 2), mutable_args_once(1, 1));
    assert_eq!((2, 2), mutable_args_once(1, 1));
    assert_eq!((2, 2), mutable_args_once(5, 6));
}

#[cfg(all(feature = "time_stores", feature = "proc_macro"))]
mod time_store_tests {
    use super::*;
    use cached::stores::TtlSortedCache;
    use cached::time::Instant;
    use cached::{CachedPeek, CachedRead};

    #[cached(
        ty = "TtlSortedCache<String, usize>",
        create = "{ TtlSortedCache::new(Duration::from_secs(60)) }",
        key = "String",
        convert = r#"{ input.to_string() }"#,
        unsync_reads = true
    )]
    fn expiring_sized_unsync_read(input: &str) -> usize {
        input.len()
    }

    #[once(ttl = 1)]
    fn slow_once_timestamp_after_body(input: u32) -> u32 {
        sleep(Duration::from_millis(1100));
        input
    }

    #[test]
    fn test_expiring_sized_unsync_read_macro() {
        assert_eq!(3, expiring_sized_unsync_read("abc"));
        assert_eq!(3, expiring_sized_unsync_read("abc"));
        let cache = EXPIRING_SIZED_UNSYNC_READ.read();
        assert_eq!(Some(&3), cache.cache_peek("abc"));
        assert_eq!(Some(&3), cache.cache_get_read("abc"));
    }

    #[test]
    #[serial]
    fn test_once_ttl_starts_after_body_finishes() {
        assert_eq!(1, slow_once_timestamp_after_body(1));
        assert_eq!(1, slow_once_timestamp_after_body(2));
    }

    #[cached(size = 1, ttl = 1)]
    fn proc_timed_sized_sleeper(n: u64) -> u64 {
        sleep(Duration::new(1, 0));
        n
    }

    #[test]
    fn test_proc_timed_sized_cache() {
        proc_timed_sized_sleeper(1);
        proc_timed_sized_sleeper(1);
        {
            let cache = PROC_TIMED_SIZED_SLEEPER.read();
            assert_eq!(1, cache.cache_misses().unwrap());
            assert_eq!(1, cache.cache_hits().unwrap());
        }
        // sleep to expire the one entry
        sleep(Duration::new(1, 0));
        proc_timed_sized_sleeper(1);
        {
            let cache = PROC_TIMED_SIZED_SLEEPER.read();
            assert_eq!(2, cache.cache_misses().unwrap());
            assert_eq!(1, cache.cache_hits().unwrap());
            assert_eq!(cache.key_order(), vec![1]);
        }
        // sleep to expire the one entry
        sleep(Duration::new(1, 0));
        {
            let cache = PROC_TIMED_SIZED_SLEEPER.read();
            assert!(cache.key_order().is_empty());
        }
        proc_timed_sized_sleeper(1);
        proc_timed_sized_sleeper(1);
        {
            let cache = PROC_TIMED_SIZED_SLEEPER.read();
            assert_eq!(3, cache.cache_misses().unwrap());
            assert_eq!(2, cache.cache_hits().unwrap());
            assert_eq!(cache.key_order(), vec![1]);
        }
        // lru size is 1, so this new thing evicts the existing key
        proc_timed_sized_sleeper(2);
        {
            let cache = PROC_TIMED_SIZED_SLEEPER.read();
            assert_eq!(4, cache.cache_misses().unwrap());
            assert_eq!(2, cache.cache_hits().unwrap());
            assert_eq!(cache.key_order(), vec![2]);
        }
    }

    /// should only cache the _first_ value returned for 1 second.
    /// all arguments are ignored for subsequent calls until the
    /// cache expires after a second.
    #[once(ttl = 1)]
    fn only_cached_once_per_second(s: String) -> Vec<String> {
        vec![s]
    }

    #[test]
    fn test_only_cached_once_per_second() {
        let a = only_cached_once_per_second("a".to_string());
        let b = only_cached_once_per_second("b".to_string());
        assert_eq!(a, b);
        sleep(Duration::new(1, 0));
        let b = only_cached_once_per_second("b".to_string());
        assert_eq!(vec!["b".to_string()], b);
    }

    /// should only cache the _first_ `Ok` returned for 1 second.
    /// all arguments are ignored for subsequent calls until the
    /// cache expires after a second.
    #[once(result = true, ttl = 1)]
    fn only_cached_result_once_per_second(
        s: String,
        error: bool,
    ) -> std::result::Result<Vec<String>, u32> {
        if error {
            Err(1)
        } else {
            Ok(vec![s])
        }
    }

    #[test]
    fn test_only_cached_result_once_per_second() {
        assert!(only_cached_result_once_per_second("z".to_string(), true).is_err());
        let a = only_cached_result_once_per_second("a".to_string(), false).unwrap();
        let b = only_cached_result_once_per_second("b".to_string(), false).unwrap();
        assert_eq!(a, b);
        sleep(Duration::new(1, 0));
        let b = only_cached_result_once_per_second("b".to_string(), false).unwrap();
        assert_eq!(vec!["b".to_string()], b);
    }

    /// should only cache the _first_ `Some` returned for 1 second.
    /// all arguments are ignored for subsequent calls until the
    /// cache expires after a second.
    #[once(option = true, ttl = 1)]
    fn only_cached_option_once_per_second(s: String, none: bool) -> Option<Vec<String>> {
        if none {
            None
        } else {
            Some(vec![s])
        }
    }

    #[test]
    fn test_only_cached_option_once_per_second() {
        assert!(only_cached_option_once_per_second("z".to_string(), true).is_none());
        let a = only_cached_option_once_per_second("a".to_string(), false).unwrap();
        let b = only_cached_option_once_per_second("b".to_string(), false).unwrap();
        assert_eq!(a, b);
        sleep(Duration::new(1, 0));
        let b = only_cached_option_once_per_second("b".to_string(), false).unwrap();
        assert_eq!(vec!["b".to_string()], b);
    }

    #[cached(ttl = 2, sync_writes = "default", key = "u32", convert = "{ 1 }")]
    fn cached_sync_writes(s: String) -> Vec<String> {
        vec![s]
    }

    #[test]
    fn test_cached_sync_writes() {
        let a = std::thread::spawn(|| cached_sync_writes("a".to_string()));
        sleep(Duration::new(1, 0));
        let b = std::thread::spawn(|| cached_sync_writes("b".to_string()));
        let c = std::thread::spawn(|| cached_sync_writes("c".to_string()));
        let a = a.join().unwrap();
        let b = b.join().unwrap();
        let c = c.join().unwrap();
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    #[cached(ttl = 2, sync_writes = true, key = "u32", convert = "{ 2 }")]
    fn cached_sync_writes_true(s: String) -> Vec<String> {
        vec![s]
    }

    #[test]
    fn test_cached_sync_writes_true() {
        let a = cached_sync_writes_true("a".to_string());
        let b = cached_sync_writes_true("b".to_string());
        assert_eq!(a, b);
    }

    #[cached(ttl = 2, sync_writes = false, key = "u32", convert = "{ 3 }")]
    fn cached_sync_writes_false(s: String) -> Vec<String> {
        vec![s]
    }

    #[test]
    fn test_cached_sync_writes_false() {
        let a = cached_sync_writes_false("a".to_string());
        let b = cached_sync_writes_false("b".to_string());
        assert_eq!(a, b);
    }

    #[cached(
        ttl = 2,
        sync_writes = "by_key",
        sync_writes_buckets = 8,
        key = "u32",
        convert = "{ 1 }"
    )]
    fn cached_sync_writes_by_key(s: String) -> Vec<String> {
        sleep(Duration::new(1, 0));
        vec![s]
    }

    #[test]
    fn test_cached_sync_writes_by_key() {
        let a = std::thread::spawn(|| cached_sync_writes_by_key("a".to_string()));
        let b = std::thread::spawn(|| cached_sync_writes_by_key("b".to_string()));
        let c = std::thread::spawn(|| cached_sync_writes_by_key("c".to_string()));
        let start = Instant::now();
        let _ = a.join().unwrap();
        let _ = b.join().unwrap();
        let _ = c.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[cached(
        ttl = 1,
        refresh = true,
        key = "String",
        convert = r#"{ String::from(s) }"#
    )]
    fn cached_timed_refresh(s: &str) -> bool {
        s == "true"
    }

    #[test]
    fn test_cached_timed_refresh() {
        assert!(cached_timed_refresh("true"));
        {
            let cache = CACHED_TIMED_REFRESH.read();
            assert_eq!(cache.cache_hits(), Some(0));
            assert_eq!(cache.cache_misses(), Some(1));
        }

        assert!(cached_timed_refresh("true"));
        {
            let cache = CACHED_TIMED_REFRESH.read();
            assert_eq!(cache.cache_hits(), Some(1));
            assert_eq!(cache.cache_misses(), Some(1));
        }

        std::thread::sleep(Duration::from_millis(500));
        assert!(cached_timed_refresh("true"));
        std::thread::sleep(Duration::from_millis(500));
        assert!(cached_timed_refresh("true"));
        std::thread::sleep(Duration::from_millis(500));
        assert!(cached_timed_refresh("true"));
        {
            let cache = CACHED_TIMED_REFRESH.read();
            assert_eq!(cache.cache_hits(), Some(4));
            assert_eq!(cache.cache_misses(), Some(1));
        }
    }

    #[cached(
        size = 2,
        ttl = 1,
        refresh = true,
        key = "String",
        convert = r#"{ String::from(s) }"#
    )]
    fn cached_timed_sized_refresh(s: &str) -> bool {
        s == "true"
    }

    #[test]
    fn test_cached_timed_sized_refresh() {
        assert!(cached_timed_sized_refresh("true"));
        {
            let cache = CACHED_TIMED_SIZED_REFRESH.read();
            assert_eq!(cache.cache_hits(), Some(0));
            assert_eq!(cache.cache_misses(), Some(1));
        }

        assert!(cached_timed_sized_refresh("true"));
        {
            let cache = CACHED_TIMED_SIZED_REFRESH.read();
            assert_eq!(cache.cache_hits(), Some(1));
            assert_eq!(cache.cache_misses(), Some(1));
        }

        std::thread::sleep(Duration::from_millis(500));
        assert!(cached_timed_sized_refresh("true"));
        std::thread::sleep(Duration::from_millis(500));
        assert!(cached_timed_sized_refresh("true"));
        std::thread::sleep(Duration::from_millis(500));
        assert!(cached_timed_sized_refresh("true"));
        {
            let cache = CACHED_TIMED_SIZED_REFRESH.read();
            assert_eq!(cache.cache_hits(), Some(4));
            assert_eq!(cache.cache_misses(), Some(1));
        }
    }

    #[cached(
        size = 2,
        ttl = 1,
        refresh = true,
        key = "String",
        convert = r#"{ String::from(s) }"#
    )]
    fn cached_timed_sized_refresh_prime(s: &str) -> bool {
        s == "true"
    }

    #[test]
    fn test_cached_timed_sized_refresh_prime() {
        assert!(cached_timed_sized_refresh_prime("true"));
        {
            let cache = CACHED_TIMED_SIZED_REFRESH_PRIME.read();
            assert_eq!(cache.cache_hits(), Some(0));
            assert_eq!(cache.cache_misses(), Some(1));
        }
        assert!(cached_timed_sized_refresh_prime("true"));
        {
            let cache = CACHED_TIMED_SIZED_REFRESH_PRIME.read();
            assert_eq!(cache.cache_hits(), Some(1));
            assert_eq!(cache.cache_misses(), Some(1));
        }

        std::thread::sleep(Duration::from_millis(500));
        assert!(cached_timed_sized_refresh_prime_prime_cache("true"));
        std::thread::sleep(Duration::from_millis(500));
        assert!(cached_timed_sized_refresh_prime_prime_cache("true"));
        std::thread::sleep(Duration::from_millis(500));
        assert!(cached_timed_sized_refresh_prime_prime_cache("true"));

        // stats unchanged (other than this new hit) since we kept priming
        assert!(cached_timed_sized_refresh_prime("true"));
        {
            let cache = CACHED_TIMED_SIZED_REFRESH_PRIME.read();
            assert_eq!(cache.cache_hits(), Some(2));
            assert_eq!(cache.cache_misses(), Some(1));
        }
    }

    #[cached(size = 2, ttl = 1, key = "String", convert = r#"{ String::from(s) }"#)]
    fn cached_timed_sized_prime(s: &str) -> bool {
        s == "true"
    }

    #[test]
    fn test_cached_timed_sized_prime() {
        assert!(cached_timed_sized_prime("true"));
        {
            let cache = CACHED_TIMED_SIZED_PRIME.write();
            assert_eq!(cache.cache_hits(), Some(0));
            assert_eq!(cache.cache_misses(), Some(1));
        }
        assert!(cached_timed_sized_prime("true"));
        {
            let cache = CACHED_TIMED_SIZED_PRIME.write();
            assert_eq!(cache.cache_hits(), Some(1));
            assert_eq!(cache.cache_misses(), Some(1));
        }

        std::thread::sleep(Duration::from_millis(500));
        assert!(cached_timed_sized_prime_prime_cache("true"));
        std::thread::sleep(Duration::from_millis(500));
        assert!(cached_timed_sized_prime_prime_cache("true"));
        std::thread::sleep(Duration::from_millis(500));
        assert!(cached_timed_sized_prime_prime_cache("true"));

        // stats unchanged (other than this new hit) since we kept priming
        assert!(cached_timed_sized_prime("true"));
        {
            let mut cache = CACHED_TIMED_SIZED_PRIME.write();
            assert_eq!(cache.cache_hits(), Some(2));
            assert_eq!(cache.cache_misses(), Some(1));
            assert!(cache.cache_size() > 0);
            std::thread::sleep(Duration::from_millis(1000));
            cache.evict();
            assert_eq!(cache.cache_size(), 0);
        }
    }

    #[cached::macros::cached(result = true, ttl = 1, result_fallback = true)]
    fn always_failing() -> Result<String, ()> {
        Err(())
    }

    #[test]
    fn test_result_fallback() {
        assert!(always_failing().is_err());
        {
            let cache = ALWAYS_FAILING.write();
            assert_eq!(cache.cache_hits(), Some(0));
            assert_eq!(cache.cache_misses(), Some(1));
        }

        // Pretend it succeeded once
        ALWAYS_FAILING.write().cache_set((), "abc".to_string());
        assert_eq!(always_failing(), Ok("abc".to_string()));
        {
            let cache = ALWAYS_FAILING.write();
            assert_eq!(cache.cache_hits(), Some(1));
            assert_eq!(cache.cache_misses(), Some(1));
        }

        std::thread::sleep(Duration::from_millis(2000));

        // Even though the cache should've expired, the `result_fallback` flag means it refreshes the cache with the last valid result
        assert_eq!(always_failing(), Ok("abc".to_string()));
        {
            let cache = ALWAYS_FAILING.write();
            assert_eq!(cache.cache_hits(), Some(1));
            assert_eq!(cache.cache_misses(), Some(2));
        }

        assert_eq!(always_failing(), Ok("abc".to_string()));
        {
            let cache = ALWAYS_FAILING.write();
            assert_eq!(cache.cache_hits(), Some(2));
            assert_eq!(cache.cache_misses(), Some(2));
        }
    }

    #[cfg(all(feature = "async", feature = "proc_macro"))]
    mod async_tests {
        use super::*;

        #[once(ttl = 1)]
        async fn only_cached_once_per_second_a(s: String) -> Vec<String> {
            vec![s]
        }

        #[tokio::test]
        async fn test_only_cached_once_per_second_a() {
            let a = only_cached_once_per_second_a("a".to_string()).await;
            let b = only_cached_once_per_second_a("b".to_string()).await;
            assert_eq!(a, b);
            sleep(Duration::new(1, 0));
            let b = only_cached_once_per_second_a("b".to_string()).await;
            assert_eq!(vec!["b".to_string()], b);
        }

        #[once(result = true, ttl = 1)]
        async fn only_cached_result_once_per_second_a(
            s: String,
            error: bool,
        ) -> std::result::Result<Vec<String>, u32> {
            if error {
                Err(1)
            } else {
                Ok(vec![s])
            }
        }

        #[tokio::test]
        async fn test_only_cached_result_once_per_second_a() {
            assert!(only_cached_result_once_per_second_a("z".to_string(), true)
                .await
                .is_err());
            let a = only_cached_result_once_per_second_a("a".to_string(), false)
                .await
                .unwrap();
            let b = only_cached_result_once_per_second_a("b".to_string(), false)
                .await
                .unwrap();
            assert_eq!(a, b);
            sleep(Duration::new(1, 0));
            let b = only_cached_result_once_per_second_a("b".to_string(), false)
                .await
                .unwrap();
            assert_eq!(vec!["b".to_string()], b);
        }

        #[once(option = true, ttl = 1)]
        async fn only_cached_option_once_per_second_a(
            s: String,
            none: bool,
        ) -> Option<Vec<String>> {
            if none {
                None
            } else {
                Some(vec![s])
            }
        }

        #[tokio::test]
        async fn test_only_cached_option_once_per_second_a() {
            assert!(only_cached_option_once_per_second_a("z".to_string(), true)
                .await
                .is_none());
            let a = only_cached_option_once_per_second_a("a".to_string(), false)
                .await
                .unwrap();
            let b = only_cached_option_once_per_second_a("b".to_string(), false)
                .await
                .unwrap();
            assert_eq!(a, b);
            sleep(Duration::new(1, 0));
            let b = only_cached_option_once_per_second_a("b".to_string(), false)
                .await
                .unwrap();
            assert_eq!(vec!["b".to_string()], b);
        }

        /// should only cache the _first_ value returned for 2 seconds.
        /// all arguments are ignored for subsequent calls until the
        /// cache expires after a second.
        /// when multiple un-cached tasks are running concurrently, only
        /// _one_ call will be "executed" and all others will be synchronized
        /// to return the cached result of the one call instead of all
        /// concurrently un-cached tasks executing and writing concurrently.
        #[once(ttl = 2, sync_writes)]
        async fn only_cached_once_per_second_sync_writes(s: String) -> Vec<String> {
            vec![s]
        }

        #[tokio::test]
        async fn test_only_cached_once_per_second_sync_writes() {
            let a = tokio::spawn(only_cached_once_per_second_sync_writes("a".to_string()));
            tokio::time::sleep(Duration::new(1, 0)).await;
            let b = tokio::spawn(only_cached_once_per_second_sync_writes("b".to_string()));
            assert_eq!(a.await.unwrap(), b.await.unwrap());
        }

        #[cached(ttl = 2, sync_writes = "default", key = "u32", convert = "{ 1 }")]
        async fn cached_sync_writes_a(s: String) -> Vec<String> {
            vec![s]
        }

        #[tokio::test]
        async fn test_cached_sync_writes_a() {
            let a = tokio::spawn(cached_sync_writes_a("a".to_string()));
            tokio::time::sleep(Duration::new(1, 0)).await;
            let b = tokio::spawn(cached_sync_writes_a("b".to_string()));
            let c = tokio::spawn(cached_sync_writes_a("c".to_string()));
            let a = a.await.unwrap();
            assert_eq!(a, b.await.unwrap());
            assert_eq!(a, c.await.unwrap());
        }

        #[cached(
            ttl = 5,
            sync_writes = "by_key",
            key = "String",
            convert = r#"{ format!("{}", s) }"#
        )]
        async fn cached_sync_writes_by_key_a(s: String) -> Vec<String> {
            tokio::time::sleep(Duration::from_secs(1)).await;
            vec![s]
        }

        #[tokio::test]
        async fn test_cached_sync_writes_by_key_a() {
            let a = tokio::spawn(cached_sync_writes_by_key_a("a".to_string()));
            let b = tokio::spawn(cached_sync_writes_by_key_a("b".to_string()));
            let c = tokio::spawn(cached_sync_writes_by_key_a("c".to_string()));
            let start = Instant::now();
            a.await.unwrap();
            b.await.unwrap();
            c.await.unwrap();
            assert!(start.elapsed() < Duration::from_secs(2));
        }
    }
}

#[cfg(all(feature = "async", feature = "proc_macro"))]
mod async_tests {
    use super::*;

    #[once(result = true)]
    async fn only_cached_result_once_a(
        s: String,
        error: bool,
    ) -> std::result::Result<Vec<String>, u32> {
        if error {
            Err(1)
        } else {
            Ok(vec![s])
        }
    }

    #[tokio::test]
    async fn test_only_cached_result_once_a() {
        assert!(only_cached_result_once_a("z".to_string(), true)
            .await
            .is_err());
        let a = only_cached_result_once_a("a".to_string(), false)
            .await
            .unwrap();
        let b = only_cached_result_once_a("b".to_string(), false)
            .await
            .unwrap();
        assert_eq!(a, b);
        sleep(Duration::new(1, 0));
        let b = only_cached_result_once_a("b".to_string(), false)
            .await
            .unwrap();
        assert_eq!(a, b);
    }

    #[once(option = true)]
    async fn only_cached_option_once_a(s: String, none: bool) -> Option<Vec<String>> {
        if none {
            None
        } else {
            Some(vec![s])
        }
    }

    #[tokio::test]
    async fn test_only_cached_option_once_a() {
        assert!(only_cached_option_once_a("z".to_string(), true)
            .await
            .is_none());
        let a = only_cached_option_once_a("a".to_string(), false)
            .await
            .unwrap();
        let b = only_cached_option_once_a("b".to_string(), false)
            .await
            .unwrap();
        assert_eq!(a, b);
        sleep(Duration::new(1, 0));
        let b = only_cached_option_once_a("b".to_string(), false)
            .await
            .unwrap();
        assert_eq!(a, b);
    }

    #[once(sync_writes = true)]
    async fn once_sync_writes_a(s: &tokio::sync::Mutex<String>) -> String {
        let mut guard = s.lock().await;
        let results: String = (*guard).clone();
        *guard = "consumed".to_string();
        results
    }

    #[tokio::test]
    async fn test_once_sync_writes_a() {
        let a_mutex = tokio::sync::Mutex::new("a".to_string());
        let b_mutex = tokio::sync::Mutex::new("b".to_string());
        let fut_a = once_sync_writes_a(&a_mutex);
        let fut_b = once_sync_writes_a(&b_mutex);
        let a = fut_a.await;
        let b = fut_b.await;
        assert_eq!(a, b);
        assert_eq!("a", a);

        // cache function was executed for a — inner string was consumed
        assert_eq!("consumed", a_mutex.lock().await.to_string());
        // cache inner was NOT executed for b (cached after first call)
        assert_eq!("b", b_mutex.lock().await.to_string());
    }
}

#[cfg(all(feature = "disk_store", feature = "proc_macro"))]
mod disk_tests {
    use super::*;
    use cached::macros::concurrent_cached;
    use cached::DiskCache;
    use thiserror::Error;

    #[derive(Error, Debug, PartialEq, Clone)]
    enum TestError {
        #[error("error with disk cache `{0}`")]
        DiskError(String),
        #[error("count `{0}`")]
        Count(u32),
    }

    #[concurrent_cached(
        disk = true,
        ttl = 1,
        map_error = r##"|e| TestError::DiskError(format!("{:?}", e))"##
    )]
    fn cached_disk(n: u32) -> Result<u32, TestError> {
        if n < 5 {
            Ok(n)
        } else {
            Err(TestError::Count(n))
        }
    }

    #[test]
    fn test_cached_disk() {
        assert_eq!(cached_disk(1), Ok(1));
        assert_eq!(cached_disk(1), Ok(1));
        assert_eq!(cached_disk(5), Err(TestError::Count(5)));
        assert_eq!(cached_disk(6), Err(TestError::Count(6)));
    }

    #[concurrent_cached(
        disk = true,
        ttl = 1,
        with_cached_flag = true,
        map_error = r##"|e| TestError::DiskError(format!("{:?}", e))"##
    )]
    fn cached_disk_cached_flag(n: u32) -> Result<cached::Return<u32>, TestError> {
        if n < 5 {
            Ok(cached::Return::new(n))
        } else {
            Err(TestError::Count(n))
        }
    }

    #[test]
    fn test_cached_disk_cached_flag() {
        assert!(!cached_disk_cached_flag(1).unwrap().was_cached);
        assert!(cached_disk_cached_flag(1).unwrap().was_cached);
        assert!(cached_disk_cached_flag(5).is_err());
        assert!(cached_disk_cached_flag(6).is_err());
    }

    #[concurrent_cached(
        map_error = r##"|e| TestError::DiskError(format!("{:?}", e))"##,
        ty = "cached::DiskCache<u32, u32>",
        create = r##" { DiskCache::new("cached_disk_cache_create").ttl(Duration::from_secs(1)).refresh(true).build().expect("error building disk cache") } "##
    )]
    fn cached_disk_cache_create(n: u32) -> Result<u32, TestError> {
        if n < 5 {
            Ok(n)
        } else {
            Err(TestError::Count(n))
        }
    }

    #[test]
    fn test_cached_disk_cache_create() {
        assert_eq!(cached_disk_cache_create(1), Ok(1));
        assert_eq!(cached_disk_cache_create(1), Ok(1));
        assert_eq!(cached_disk_cache_create(5), Err(TestError::Count(5)));
        assert_eq!(cached_disk_cache_create(6), Err(TestError::Count(6)));
    }

    /// Just calling the macro with connection_config to test it doesn't break with an expected string
    /// for connection_config.
    /// There are no simple tests to test this here
    #[concurrent_cached(
        disk = true,
        map_error = r##"|e| TestError::DiskError(format!("{:?}", e))"##,
        connection_config = r##"sled::Config::new().flush_every_ms(None)"##
    )]
    fn cached_disk_connection_config(n: u32) -> Result<u32, TestError> {
        if n < 5 {
            Ok(n)
        } else {
            Err(TestError::Count(n))
        }
    }

    /// Just calling the macro with sync_to_disk_on_cache_change to test it doesn't break with an expected value
    /// There are no simple tests to test this here
    #[concurrent_cached(
        disk = true,
        map_error = r##"|e| TestError::DiskError(format!("{:?}", e))"##,
        sync_to_disk_on_cache_change = true
    )]
    fn cached_disk_sync_to_disk_on_cache_change(n: u32) -> Result<u32, TestError> {
        if n < 5 {
            Ok(n)
        } else {
            Err(TestError::Count(n))
        }
    }

    #[cfg(all(feature = "async", feature = "proc_macro"))]
    mod async_test {
        use super::*;

        #[concurrent_cached(
            disk = true,
            map_error = r##"|e| TestError::DiskError(format!("{:?}", e))"##
        )]
        async fn async_cached_disk(n: u32) -> Result<u32, TestError> {
            if n < 5 {
                Ok(n)
            } else {
                Err(TestError::Count(n))
            }
        }

        #[tokio::test]
        async fn test_async_cached_disk() {
            assert_eq!(async_cached_disk(1).await, Ok(1));
            assert_eq!(async_cached_disk(1).await, Ok(1));
            assert_eq!(async_cached_disk(5).await, Err(TestError::Count(5)));
            assert_eq!(async_cached_disk(6).await, Err(TestError::Count(6)));
        }

        // Regression: a value that is `Send` + `Serialize` + `Clone` but **not
        // `Sync`** (it contains a `Cell`) must be usable with async disk
        // caching. Before relaxing the async `DiskCache` impl (fn-pointer
        // phantom + dropping the `V: Sync` bound) this failed to compile with
        // "future cannot be sent between threads safely".
        #[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
        struct NotSyncValue {
            c: std::cell::Cell<u32>,
        }

        #[concurrent_cached(
            disk = true,
            map_error = r##"|e| TestError::DiskError(format!("{:?}", e))"##
        )]
        async fn async_cached_disk_not_sync(n: u32) -> Result<NotSyncValue, TestError> {
            Ok(NotSyncValue {
                c: std::cell::Cell::new(n),
            })
        }

        #[tokio::test]
        async fn test_async_cached_disk_not_sync_value() {
            fn assert_send<T: Send>() {}
            assert_send::<NotSyncValue>(); // Send but (via Cell) !Sync
            assert_eq!(
                async_cached_disk_not_sync(7).await.unwrap(),
                NotSyncValue {
                    c: std::cell::Cell::new(7)
                }
            );
            // second call is served from the disk cache
            assert_eq!(
                async_cached_disk_not_sync(7).await.unwrap(),
                NotSyncValue {
                    c: std::cell::Cell::new(7)
                }
            );
        }
    }
}

// Regression (P2): a value that is `Send + Serialize + Clone` but **not
// `Sync`** (contains a `Cell`) must be usable with Redis-backed caches. Before
// the fn-pointer `PhantomData` on `RedisCache`/`AsyncRedisCache` and the
// dropped `V: Sync` bound on the async `AsyncRedisCache::new` / `impl
// ConcurrentCachedAsync` blocks, the sync path failed because the macro-emitted
// `LazyLock<RwLock<RedisCache<_, V>>>` static required `RedisCache: Sync`
// (which `PhantomData<(K, V)>` propagated from `V: Sync`), and the async path
// failed at the explicit `V: Send + Sync` bound. Compile-only — no server
// required.
#[cfg(feature = "redis_store")]
#[allow(dead_code)]
mod redis_not_sync_typecheck {
    #[derive(serde::Serialize, serde::Deserialize, Clone)]
    struct NotSyncValue {
        c: std::cell::Cell<u32>,
    }

    fn _typecheck_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<cached::RedisCache<String, NotSyncValue>>();
        assert_sync::<cached::RedisCache<String, NotSyncValue>>();
    }

    #[cfg(any(feature = "redis_tokio", feature = "redis_smol"))]
    fn _typecheck_async() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<cached::AsyncRedisCache<String, NotSyncValue>>();
        assert_sync::<cached::AsyncRedisCache<String, NotSyncValue>>();
    }
}

// Regression (P2): an error type literally named `Return` must not be
// misclassified by `#[concurrent_cached]` as `cached::Return<T>`. Without
// `with_cached_flag`, the cache value type is the `Result` Ok type (`String`).
// Before the fix this failed to expand with "unable to determine cache value
// type".
#[cfg(feature = "proc_macro")]
mod concurrent_cached_return_named_error {
    use cached::macros::concurrent_cached;

    #[derive(Debug, PartialEq)]
    struct Return; // intentionally shadows `cached::Return` within this module

    struct Store(std::sync::Mutex<std::collections::HashMap<String, String>>);
    impl Store {
        fn new() -> Self {
            Self(std::sync::Mutex::new(std::collections::HashMap::new()))
        }
    }
    impl cached::ConcurrentCached<String, String> for Store {
        type Error = std::convert::Infallible;
        fn cache_get(&self, k: &String) -> Result<Option<String>, Self::Error> {
            Ok(self.0.lock().unwrap().get(k).cloned())
        }
        fn cache_set(&self, k: String, v: String) -> Result<Option<String>, Self::Error> {
            Ok(self.0.lock().unwrap().insert(k, v))
        }
        fn cache_remove(&self, k: &String) -> Result<Option<String>, Self::Error> {
            Ok(self.0.lock().unwrap().remove(k))
        }
        fn set_refresh_on_hit(&mut self, _r: bool) -> bool {
            false
        }
    }

    #[concurrent_cached(
        ty = "Store",
        create = "{ Store::new() }",
        key = "String",
        convert = r#"{ k.to_string() }"#,
        map_error = r#"|_e| Return"#
    )]
    fn fetch(k: u32) -> Result<String, Return> {
        Ok(k.to_string())
    }

    #[test]
    fn return_named_error_compiles_and_caches() {
        assert_eq!(fetch(1), Ok("1".to_string()));
        assert_eq!(fetch(1), Ok("1".to_string())); // cached
        assert_eq!(fetch(2), Ok("2".to_string()));
    }
}

#[cfg(all(feature = "redis_store", feature = "proc_macro"))]
mod redis_tests {
    use super::*;
    use cached::macros::concurrent_cached;
    use cached::RedisCache;
    use thiserror::Error;

    #[derive(Error, Debug, PartialEq, Clone)]
    enum TestError {
        #[error("error with redis cache `{0}`")]
        RedisError(String),
        #[error("count `{0}`")]
        Count(u32),
    }

    #[concurrent_cached(
        redis = true,
        ttl = 1,
        cache_prefix_block = "{ \"__cached_redis_proc_macro_test_fn_cached_redis\" }",
        map_error = r##"|e| TestError::RedisError(format!("{:?}", e))"##
    )]
    fn cached_redis(n: u32) -> Result<u32, TestError> {
        if n < 5 {
            Ok(n)
        } else {
            Err(TestError::Count(n))
        }
    }

    #[test]
    fn test_cached_redis() {
        assert_eq!(cached_redis(1), Ok(1));
        assert_eq!(cached_redis(1), Ok(1));
        assert_eq!(cached_redis(5), Err(TestError::Count(5)));
        assert_eq!(cached_redis(6), Err(TestError::Count(6)));
    }

    #[concurrent_cached(
        redis = true,
        ttl = 1,
        with_cached_flag = true,
        map_error = r##"|e| TestError::RedisError(format!("{:?}", e))"##
    )]
    fn cached_redis_cached_flag(n: u32) -> Result<cached::Return<u32>, TestError> {
        if n < 5 {
            Ok(cached::Return::new(n))
        } else {
            Err(TestError::Count(n))
        }
    }

    #[test]
    fn test_cached_redis_cached_flag() {
        assert!(!cached_redis_cached_flag(1).unwrap().was_cached);
        assert!(cached_redis_cached_flag(1).unwrap().was_cached);
        assert!(cached_redis_cached_flag(5).is_err());
        assert!(cached_redis_cached_flag(6).is_err());
    }

    #[concurrent_cached(
        map_error = r##"|e| TestError::RedisError(format!("{:?}", e))"##,
        ty = "cached::RedisCache<u32, u32>",
        create = r##" { RedisCache::new("cache_redis_test_cache_create", Duration::from_secs(1)).refresh(true).build().expect("error building redis cache") } "##
    )]
    fn cached_redis_cache_create(n: u32) -> Result<u32, TestError> {
        if n < 5 {
            Ok(n)
        } else {
            Err(TestError::Count(n))
        }
    }

    #[test]
    fn test_cached_redis_cache_create() {
        assert_eq!(cached_redis_cache_create(1), Ok(1));
        assert_eq!(cached_redis_cache_create(1), Ok(1));
        assert_eq!(cached_redis_cache_create(5), Err(TestError::Count(5)));
        assert_eq!(cached_redis_cache_create(6), Err(TestError::Count(6)));
    }

    #[cfg(any(feature = "redis_smol", feature = "redis_tokio"))]
    mod async_redis_tests {
        use super::*;

        #[concurrent_cached(
            redis = true,
            ttl = 1,
            cache_prefix_block = "{ \"__cached_redis_proc_macro_test_fn_async_cached_redis\" }",
            map_error = r##"|e| TestError::RedisError(format!("{:?}", e))"##
        )]
        async fn async_cached_redis(n: u32) -> Result<u32, TestError> {
            if n < 5 {
                Ok(n)
            } else {
                Err(TestError::Count(n))
            }
        }

        #[tokio::test]
        async fn test_async_cached_redis() {
            assert_eq!(async_cached_redis(1).await, Ok(1));
            assert_eq!(async_cached_redis(1).await, Ok(1));
            assert_eq!(async_cached_redis(5).await, Err(TestError::Count(5)));
            assert_eq!(async_cached_redis(6).await, Err(TestError::Count(6)));
        }

        #[concurrent_cached(
            redis = true,
            ttl = 1,
            with_cached_flag = true,
            map_error = r##"|e| TestError::RedisError(format!("{:?}", e))"##
        )]
        async fn async_cached_redis_cached_flag(n: u32) -> Result<cached::Return<u32>, TestError> {
            if n < 5 {
                Ok(cached::Return::new(n))
            } else {
                Err(TestError::Count(n))
            }
        }

        #[tokio::test]
        async fn test_async_cached_redis_cached_flag() {
            assert!(!async_cached_redis_cached_flag(1).await.unwrap().was_cached);
            assert!(async_cached_redis_cached_flag(1).await.unwrap().was_cached,);
            assert!(async_cached_redis_cached_flag(5).await.is_err());
            assert!(async_cached_redis_cached_flag(6).await.is_err());
        }

        use cached::AsyncRedisCache;
        #[concurrent_cached(
            map_error = r##"|e| TestError::RedisError(format!("{:?}", e))"##,
            ty = "cached::AsyncRedisCache<u32, u32>",
            create = r##" { AsyncRedisCache::new("async_cached_redis_test_cache_create", Duration::from_secs(1)).refresh(true).build().await.expect("error building async redis cache") } "##
        )]
        async fn async_cached_redis_cache_create(n: u32) -> Result<u32, TestError> {
            if n < 5 {
                Ok(n)
            } else {
                Err(TestError::Count(n))
            }
        }

        #[tokio::test]
        async fn test_async_cached_redis_cache_create() {
            assert_eq!(async_cached_redis_cache_create(1).await, Ok(1));
            assert_eq!(async_cached_redis_cache_create(1).await, Ok(1));
            assert_eq!(
                async_cached_redis_cache_create(5).await,
                Err(TestError::Count(5))
            );
            assert_eq!(
                async_cached_redis_cache_create(6).await,
                Err(TestError::Count(6))
            );
        }
    }
}

#[cfg(feature = "proc_macro")]
#[derive(Clone)]
pub struct NewsArticle {
    slug: String,
    is_expired: bool,
}

#[cfg(feature = "proc_macro")]
impl Expires for NewsArticle {
    fn is_expired(&self) -> bool {
        self.is_expired
    }
}

#[cfg(feature = "proc_macro")]
const EXPIRED_SLUG: &str = "expired_slug";
#[cfg(feature = "proc_macro")]
const UNEXPIRED_SLUG: &str = "unexpired_slug";

#[cfg(feature = "proc_macro")]
#[cached(
    ty = "ExpiringLruCache<String, NewsArticle>",
    create = "{ ExpiringLruCache::with_size(3) }",
    result = true
)]
fn fetch_article(slug: String) -> Result<NewsArticle, ()> {
    match slug.as_str() {
        EXPIRED_SLUG => Ok(NewsArticle {
            slug: String::from(EXPIRED_SLUG),
            is_expired: true,
        }),
        UNEXPIRED_SLUG => Ok(NewsArticle {
            slug: String::from(UNEXPIRED_SLUG),
            is_expired: false,
        }),
        _ => Err(()),
    }
}

#[cfg(feature = "proc_macro")]
#[test]
#[serial(ExpiringCacheTest)]
fn test_expiring_value_expired_article_returned_with_miss() {
    {
        let mut cache = FETCH_ARTICLE.write();
        cache.cache_reset();
        cache.cache_reset_metrics();
    }
    let expired_article = fetch_article(EXPIRED_SLUG.to_string());

    assert!(expired_article.is_ok());
    assert_eq!(EXPIRED_SLUG, expired_article.unwrap().slug.as_str());

    // The article was fetched due to a cache miss and the result cached.
    {
        let cache = FETCH_ARTICLE.write();
        assert_eq!(1, cache.cache_size());
        assert_eq!(cache.cache_hits(), Some(0));
        assert_eq!(cache.cache_misses(), Some(1));
    }

    let _ = fetch_article(EXPIRED_SLUG.to_string());

    // The article was fetched again as it had expired.
    {
        let cache = FETCH_ARTICLE.write();
        assert_eq!(1, cache.cache_size());
        assert_eq!(cache.cache_hits(), Some(0));
        assert_eq!(cache.cache_misses(), Some(2));
    }
}

#[cfg(feature = "proc_macro")]
#[test]
#[serial(ExpiringCacheTest)]
fn test_expiring_value_unexpired_article_returned_with_hit() {
    {
        let mut cache = FETCH_ARTICLE.write();
        cache.cache_reset();
        cache.cache_reset_metrics();
    }
    let unexpired_article = fetch_article(UNEXPIRED_SLUG.to_string());

    assert!(unexpired_article.is_ok());
    assert_eq!(UNEXPIRED_SLUG, unexpired_article.unwrap().slug.as_str());

    // The article was fetched due to a cache miss and the result cached.
    {
        let cache = FETCH_ARTICLE.write();
        assert_eq!(1, cache.cache_size());
        assert_eq!(cache.cache_hits(), Some(0));
        assert_eq!(cache.cache_misses(), Some(1));
    }

    let cached_article = fetch_article(UNEXPIRED_SLUG.to_string());
    assert!(cached_article.is_ok());
    assert_eq!(UNEXPIRED_SLUG, cached_article.unwrap().slug.as_str());

    // The article was not fetched but returned as a hit from the cache.
    {
        let cache = FETCH_ARTICLE.write();
        assert_eq!(1, cache.cache_size());
        assert_eq!(cache.cache_hits(), Some(1));
        assert_eq!(cache.cache_misses(), Some(1));
    }
}

#[test]
fn test_sized_cache_on_evict() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::LruCache::builder()
        .size(2)
        .on_evict(move |_k, _v| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build();

    cache.set(1, 10);
    cache.set(2, 20);
    assert_eq!(evicted_count.load(Ordering::Relaxed), 0);
    cache.set(3, 30);
    assert_eq!(evicted_count.load(Ordering::Relaxed), 1);
}

#[test]
#[cfg(feature = "time_stores")]
fn test_timed_cache_on_evict() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::TtlCache::builder()
        .ttl(cached::time::Duration::from_millis(100))
        .on_evict(move |_k, _v| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build();

    cache.set(1, 10);
    assert_eq!(evicted_count.load(Ordering::Relaxed), 0);
    std::thread::sleep(cached::time::Duration::from_millis(200));
    assert_eq!(cache.evict(), 1);
    assert_eq!(evicted_count.load(Ordering::Relaxed), 1);
    assert_eq!(cache.cache_evictions(), Some(1));
}

#[test]
#[cfg(feature = "time_stores")]
fn test_cache_evict_trait_returns_count() {
    use cached::CacheEvict;

    let mut cache = cached::TtlCache::with_ttl(cached::time::Duration::from_millis(20));
    cache.cache_set(1, 10);
    cache.cache_set(2, 20);
    std::thread::sleep(cached::time::Duration::from_millis(40));

    assert_eq!(CacheEvict::evict(&mut cache), 2);
    assert_eq!(cache.cache_size(), 0);
    assert_eq!(cache.cache_evictions(), Some(2));
}

#[test]
#[cfg(feature = "time_stores")]
fn test_expiring_sized_cache_on_evict() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::stores::TtlSortedCache::builder()
        .size(2)
        .ttl(cached::time::Duration::from_secs(10))
        .on_evict(move |_k, _v| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build();

    cache.set(1, 10);
    cache.set(2, 20);
    assert_eq!(evicted_count.load(Ordering::Relaxed), 0);
    cache.set(3, 30);
    // TtlSortedCache evicts on insert if size limit reached
    assert_eq!(evicted_count.load(Ordering::Relaxed), 1);
}

#[test]
#[cfg(feature = "time_stores")]
fn test_timed_sized_expired_get_does_not_pollute_inner_metrics() {
    let mut cache =
        cached::LruTtlCache::with_size_and_ttl(2, cached::time::Duration::from_millis(20));
    cache.cache_set(1, 10);
    cache.cache_set(2, 20);
    cache.cache_reset_metrics();
    std::thread::sleep(cached::time::Duration::from_millis(40));

    assert!(cache.cache_get(&1).is_none());
    assert_eq!(cache.cache_hits(), Some(0));
    assert_eq!(cache.cache_misses(), Some(1));
    assert_eq!(cache.store().cache_hits(), Some(0));
    assert_eq!(cache.store().cache_misses(), Some(0));
}

#[test]
#[cfg(feature = "time_stores")]
fn test_timed_sized_cache_expired_get_or_set_invokes_on_evict() {
    use cached::Cached;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::LruTtlCache::builder()
        .size(4)
        .ttl(cached::time::Duration::from_millis(50))
        .on_evict(move |_k, _v| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build();

    // Insert an entry, let it expire, then replace it via cache_get_or_set_with.
    cache.cache_set(1, 10);
    assert_eq!(evicted_count.load(Ordering::Relaxed), 0);
    assert_eq!(cache.cache_evictions(), Some(0));

    std::thread::sleep(cached::time::Duration::from_millis(100));

    // This should detect the expired entry, fire on_evict for it, then store the new value.
    let val = cache.cache_get_or_set_with(1, || 99);
    assert_eq!(*val, 99);
    assert_eq!(
        evicted_count.load(Ordering::Relaxed),
        1,
        "on_evict must fire for expired replacement"
    );
    // The outer evictions counter must include the expired-replacement eviction.
    assert!(
        cache.cache_evictions().unwrap() >= 1,
        "cache_evictions must be at least 1 after expired replacement"
    );
}

#[test]
fn test_expiring_value_cache_expired_get_or_set_invokes_on_evict() {
    use cached::Cached;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[derive(Clone)]
    struct Expirable {
        value: i32,
        expired: bool,
    }
    impl cached::Expires for Expirable {
        fn is_expired(&self) -> bool {
            self.expired
        }
    }

    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::ExpiringLruCache::builder()
        .size(4)
        .on_evict(move |_k: &i32, _v: &Expirable| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build();

    cache.cache_set(
        1,
        Expirable {
            value: 10,
            expired: true,
        },
    );
    assert_eq!(evicted_count.load(Ordering::Relaxed), 0);
    assert_eq!(cache.cache_evictions(), Some(0));

    // Replace the expired entry via cache_get_or_set_with.
    let val = cache.cache_get_or_set_with(1, || Expirable {
        value: 99,
        expired: false,
    });
    assert_eq!(val.value, 99);
    assert_eq!(
        evicted_count.load(Ordering::Relaxed),
        1,
        "on_evict must fire for expired replacement"
    );
    assert!(
        cache.cache_evictions().unwrap() >= 1,
        "cache_evictions must be at least 1 after expired replacement"
    );
}

#[test]
fn test_expiring_value_cache_get_mut_expired_invokes_on_evict() {
    use cached::Cached;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[derive(Clone)]
    struct Expirable {
        expired: bool,
    }
    impl cached::Expires for Expirable {
        fn is_expired(&self) -> bool {
            self.expired
        }
    }

    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::ExpiringLruCache::builder()
        .size(4)
        .on_evict(move |_k: &i32, _v: &Expirable| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build();

    cache.cache_set(1, Expirable { expired: true });
    assert!(cache.cache_get_mut(&1).is_none());
    assert_eq!(evicted_count.load(Ordering::Relaxed), 1);
    assert_eq!(cache.cache_evictions(), Some(1));
}

#[test]
fn test_fallible_builders_return_build_error() {
    struct Expirable;
    impl cached::Expires for Expirable {
        fn is_expired(&self) -> bool {
            false
        }
    }

    let sized = cached::LruCache::<i32, i32>::builder().try_build();
    assert!(
        matches!(
            sized.unwrap_err(),
            cached::BuildError::MissingRequired("size")
        ),
        "expected MissingRequired(size)"
    );

    let expiring = cached::ExpiringLruCache::<i32, Expirable>::builder().try_build();
    assert!(
        matches!(
            expiring.unwrap_err(),
            cached::BuildError::MissingRequired("size")
        ),
        "expected MissingRequired(size)"
    );

    #[cfg(feature = "time_stores")]
    {
        let timed = cached::TtlCache::<i32, i32>::builder().try_build();
        assert!(
            matches!(
                timed.unwrap_err(),
                cached::BuildError::MissingRequired("ttl")
            ),
            "expected MissingRequired(ttl)"
        );

        let timed_sized = cached::LruTtlCache::<i32, i32>::builder()
            .ttl(cached::time::Duration::from_secs(1))
            .try_build();
        assert!(
            matches!(
                timed_sized.unwrap_err(),
                cached::BuildError::MissingRequired("size")
            ),
            "expected MissingRequired(size)"
        );
    }
}

#[test]
fn test_expiring_value_cache_get_does_not_promote_expired_key() {
    use cached::{Cached, CachedPeek};

    #[derive(Clone)]
    struct Expirable {
        expired: bool,
    }
    impl cached::Expires for Expirable {
        fn is_expired(&self) -> bool {
            self.expired
        }
    }

    // Size-2 cache: insert a live entry, then an expired entry.
    // Probing the expired entry via cache_get must not promote it to most-recent.
    let mut cache = cached::ExpiringLruCache::builder().size(2).build();

    cache.cache_set(1, Expirable { expired: false }); // live, inserted first (older)
    cache.cache_set(2, Expirable { expired: true }); // expired, inserted second (newer)

    // Probing the expired key must return None and remove it, not promote it.
    assert!(cache.cache_get(&2).is_none());

    // Now insert a third entry. The expired key was already removed so the live
    // entry (key=1) must still be present.
    cache.cache_set(3, Expirable { expired: false });
    assert!(
        cache.cache_peek(&1).is_some(),
        "live entry must survive after expired entry is removed by cache_get"
    );
}

#[test]
#[cfg(feature = "time_stores")]
fn test_timed_cache_on_evict_fires_on_cache_get() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::TtlCache::builder()
        .ttl(cached::time::Duration::from_millis(50))
        .on_evict(move |_k: &u32, _v: &u32| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build();

    cache.cache_set(1, 10);
    std::thread::sleep(cached::time::Duration::from_millis(100));

    assert!(cache.cache_get(&1).is_none());
    assert_eq!(
        evicted_count.load(Ordering::Relaxed),
        1,
        "on_evict must fire when cache_get encounters an expired entry"
    );
    assert_eq!(cache.cache_evictions(), Some(1));
}

#[test]
#[cfg(feature = "time_stores")]
fn test_timed_cache_on_evict_fires_on_cache_get_or_set() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::TtlCache::builder()
        .ttl(cached::time::Duration::from_millis(50))
        .on_evict(move |_k: &u32, _v: &u32| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build();

    cache.cache_set(1, 10);
    std::thread::sleep(cached::time::Duration::from_millis(100));

    let val = cache.cache_get_or_set_with(1, || 99);
    assert_eq!(*val, 99);
    assert_eq!(
        evicted_count.load(Ordering::Relaxed),
        1,
        "on_evict must fire when cache_get_or_set_with replaces an expired entry"
    );
    assert_eq!(cache.cache_evictions(), Some(1));
}

#[cfg(all(feature = "time_stores", feature = "async"))]
#[tokio::test]
async fn test_timed_cache_async_on_evict_fires() {
    use cached::CachedAsync;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::TtlCache::builder()
        .ttl(cached::time::Duration::from_millis(50))
        .on_evict(move |_k: &u32, _v: &u32| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build();

    cache.cache_set(1, 10);
    tokio::time::sleep(cached::time::Duration::from_millis(100)).await;

    let val = CachedAsync::async_get_or_set_with(&mut cache, 1, || async { 99u32 }).await;
    assert_eq!(*val, 99);
    assert_eq!(
        evicted_count.load(Ordering::Relaxed),
        1,
        "on_evict must fire in async get_or_set_with when replacing an expired entry"
    );
    assert_eq!(cache.cache_evictions(), Some(1));
}

#[test]
#[cfg(feature = "time_stores")]
fn test_expiring_sized_cache_get_evicts_expired_and_fires_on_evict() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::stores::TtlSortedCache::builder()
        .size(4)
        .ttl(cached::time::Duration::from_millis(50))
        .on_evict(move |_k: &u32, _v: &u32| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build();

    cache.set(1, 10);
    cache.set(2, 20);
    std::thread::sleep(cached::time::Duration::from_millis(100));

    assert!(cache.cache_get(&1).is_none());
    assert_eq!(
        evicted_count.load(Ordering::Relaxed),
        1,
        "on_evict must fire when cache_get encounters an expired TtlSortedCache entry"
    );
    assert_eq!(cache.cache_evictions(), Some(1));
    assert_eq!(
        cache.cache_size(),
        1,
        "expired entry must be removed from map"
    );
}

#[test]
#[cfg(feature = "time_stores")]
fn test_timed_sized_cache_on_evict_fires_on_cache_get() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::LruTtlCache::builder()
        .size(4)
        .ttl(cached::time::Duration::from_millis(50))
        .on_evict(move |_k: &u32, _v: &u32| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build();

    cache.cache_set(1, 10);
    std::thread::sleep(cached::time::Duration::from_millis(100));

    assert!(cache.cache_get(&1).is_none());
    assert_eq!(
        evicted_count.load(Ordering::Relaxed),
        1,
        "on_evict must fire when LruTtlCache::cache_get encounters an expired entry"
    );
    assert_eq!(cache.cache_evictions(), Some(1));
}

#[test]
fn test_expiring_value_cache_on_evict_fires_on_cache_get() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[derive(Clone)]
    struct Expirable {
        expired: bool,
    }
    impl cached::Expires for Expirable {
        fn is_expired(&self) -> bool {
            self.expired
        }
    }

    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::ExpiringLruCache::builder()
        .size(4)
        .on_evict(move |_k: &i32, _v: &Expirable| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build();

    cache.cache_set(1, Expirable { expired: true });
    assert_eq!(evicted_count.load(Ordering::Relaxed), 0);

    assert!(cache.cache_get(&1).is_none());
    assert_eq!(
        evicted_count.load(Ordering::Relaxed),
        1,
        "on_evict must fire when ExpiringLruCache::cache_get encounters an expired entry"
    );
    assert_eq!(cache.cache_evictions(), Some(1));
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_unsync_reads_unbound_cache() {
    UNSYNC_DOUBLE.write().cache_reset();
    assert_eq!(4, unsync_double(2));
    assert_eq!(4, unsync_double(2));
    assert_eq!(10, unsync_double(5));
    let cache = UNSYNC_DOUBLE.read();
    assert_eq!(2, cache.cache_size());
    assert_eq!(1, cache.cache_hits().unwrap());
    assert_eq!(2, cache.cache_misses().unwrap());
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_unsync_reads_sync_writes_default_counts_single_miss() {
    UNSYNC_DOUBLE_SYNC_WRITES.write().cache_reset();
    assert_eq!(4, unsync_double_sync_writes(2));
    assert_eq!(4, unsync_double_sync_writes(2));

    let cache = UNSYNC_DOUBLE_SYNC_WRITES.read();
    assert_eq!(1, cache.cache_size());
    assert_eq!(1, cache.cache_hits().unwrap());
    assert_eq!(1, cache.cache_misses().unwrap());
}

// unsync_reads backed by TtlSortedCache (implements CachedRead)
#[cfg(all(feature = "proc_macro", feature = "time_stores"))]
mod unsync_reads_ttl_sorted {
    use cached::macros::cached;
    use cached::stores::TtlSortedCache;
    use cached::Cached;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    static CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

    #[cached(
        ty = "TtlSortedCache<String, u32>",
        create = "{ TtlSortedCache::new(Duration::from_secs(60)) }",
        convert = r#"{ format!("{}", n) }"#,
        unsync_reads = true
    )]
    fn unsync_ttl_sorted(n: u32) -> u32 {
        CALL_COUNT.fetch_add(1, Ordering::SeqCst);
        n * 3
    }

    #[test]
    fn test_unsync_reads_ttl_sorted_cache() {
        CALL_COUNT.store(0, Ordering::SeqCst);
        UNSYNC_TTL_SORTED.write().cache_reset();

        assert_eq!(unsync_ttl_sorted(4), 12);
        assert_eq!(unsync_ttl_sorted(4), 12); // cache hit — body not re-run
        assert_eq!(unsync_ttl_sorted(5), 15);

        assert_eq!(CALL_COUNT.load(Ordering::SeqCst), 2);
        let cache = UNSYNC_TTL_SORTED.read();
        assert_eq!(cache.cache_size(), 2);
        assert_eq!(cache.cache_hits(), Some(1));
        assert_eq!(cache.cache_misses(), Some(2));
    }
}

// ── Phase 8 new tests ────────────────────────────────────────────────────────

#[cfg(feature = "time_stores")]
#[test]
fn test_ttl_cache_zero_ttl() {
    use cached::TtlCache;
    let mut cache = TtlCache::with_ttl(Duration::from_nanos(0));
    cache.cache_set(1u32, "hello");
    // With zero TTL every entry expires immediately.
    assert!(cache.cache_get(&1u32).is_none());
    assert_eq!(cache.cache_misses(), Some(1));
}

#[cfg(feature = "time_stores")]
#[test]
fn test_lru_ttl_cache_zero_ttl() {
    use cached::LruTtlCache;
    let mut cache = LruTtlCache::with_size_and_ttl(4, Duration::from_nanos(0));
    cache.cache_set(1u32, "hello");
    assert!(cache.cache_get(&1u32).is_none());
    assert_eq!(cache.cache_misses(), Some(1));
}

#[cfg(feature = "time_stores")]
#[test]
fn test_ttl_sorted_cache_try_set_time_bounds() {
    use cached::stores::TtlSortedCache;
    use cached::Cached;
    // A near-maximum TTL triggers TimeBounds overflow on some platforms.
    // cache_set silently no-ops; cache_try_set returns Err.
    let ttl = Duration::from_secs(u64::MAX / 2);
    let mut cache = TtlSortedCache::<u32, u32>::new(ttl);
    // cache_set must not panic
    cache.cache_set(1, 42);
    // cache_try_set must surface the error
    let result = cache.cache_try_set(2, 99);
    // The result is either Ok (if no overflow) or Err (if overflow occurred).
    // Either is valid — the important thing is it does not panic.
    let _ = result;
}

#[test]
fn test_cache_reset_also_resets_metrics() {
    use cached::Cached;

    let mut c = UnboundCache::new();
    c.cache_set(1u32, 1u32);
    c.cache_get(&1u32);
    c.cache_get(&99u32);
    assert_eq!(c.cache_hits(), Some(1));
    assert_eq!(c.cache_misses(), Some(1));
    c.cache_reset();
    assert_eq!(c.cache_hits(), Some(0));
    assert_eq!(c.cache_misses(), Some(0));
    assert_eq!(c.cache_size(), 0);

    let mut lru = LruCache::with_size(4);
    lru.cache_set(1u32, 1u32);
    lru.cache_get(&1u32);
    lru.cache_get(&99u32);
    assert_eq!(lru.cache_hits(), Some(1));
    assert_eq!(lru.cache_misses(), Some(1));
    lru.cache_reset();
    assert_eq!(lru.cache_hits(), Some(0));
    assert_eq!(lru.cache_misses(), Some(0));
    assert_eq!(lru.cache_size(), 0);
}

#[cfg(feature = "time_stores")]
#[test]
fn test_cache_reset_also_resets_metrics_time_stores() {
    use cached::{Cached, LruTtlCache, TtlCache};

    let mut tc = TtlCache::<u32, u32>::with_ttl(Duration::from_secs(60));
    tc.cache_set(1, 1);
    tc.cache_get(&1);
    tc.cache_get(&99);
    tc.cache_reset();
    assert_eq!(tc.cache_hits(), Some(0));
    assert_eq!(tc.cache_misses(), Some(0));
    assert_eq!(tc.cache_size(), 0);

    let mut ltu = LruTtlCache::<u32, u32>::with_size_and_ttl(4, Duration::from_secs(60));
    ltu.cache_set(1, 1);
    ltu.cache_get(&1);
    ltu.cache_get(&99);
    ltu.cache_reset();
    assert_eq!(ltu.cache_hits(), Some(0));
    assert_eq!(ltu.cache_misses(), Some(0));
    assert_eq!(ltu.cache_size(), 0);
}

#[test]
fn test_cache_clear_preserves_metrics() {
    use cached::Cached;

    let mut c = UnboundCache::new();
    c.cache_set(1u32, 1u32);
    c.cache_get(&1u32);
    c.cache_get(&99u32);
    assert_eq!(c.cache_hits(), Some(1));
    assert_eq!(c.cache_misses(), Some(1));
    c.cache_clear();
    assert_eq!(c.cache_size(), 0);
    assert_eq!(c.cache_hits(), Some(1));
    assert_eq!(c.cache_misses(), Some(1));

    let mut lru = LruCache::with_size(4);
    lru.cache_set(1u32, 1u32);
    lru.cache_get(&1u32);
    lru.cache_get(&99u32);
    lru.cache_clear();
    assert_eq!(lru.cache_size(), 0);
    assert_eq!(lru.cache_hits(), Some(1));
    assert_eq!(lru.cache_misses(), Some(1));
}

#[test]
fn test_unbound_cache_on_evict_fires_on_remove() {
    use cached::Cached;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    let fired = Arc::new(AtomicU32::new(0));
    let fired_clone = fired.clone();
    let mut cache = UnboundCache::<u32, u32>::builder()
        .on_evict(move |_, _| {
            fired_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build();

    cache.cache_set(1, 100);
    cache.cache_set(2, 200);
    assert_eq!(fired.load(Ordering::Relaxed), 0);

    cache.cache_remove(&1u32);
    assert_eq!(fired.load(Ordering::Relaxed), 1);

    cache.cache_remove(&99u32); // not present — on_evict should NOT fire
    assert_eq!(fired.load(Ordering::Relaxed), 1);

    cache.cache_remove(&2u32);
    assert_eq!(fired.load(Ordering::Relaxed), 2);
}

#[cfg(feature = "time_stores")]
#[test]
fn test_lru_ttl_cache_retain() {
    use cached::{Cached, LruTtlCache};

    let mut cache = LruTtlCache::<u32, u32>::with_size_and_ttl(10, Duration::from_secs(60));
    cache.cache_set(1, 11); // odd
    cache.cache_set(2, 20); // even
    cache.cache_set(3, 31); // odd
    cache.cache_set(4, 40); // even

    // Keep only even values
    cache.retain(|_, v| v % 2 == 0);

    assert!(cache.cache_get(&1).is_none()); // 11 is odd, removed
    assert!(cache.cache_get(&2).is_some()); // 20 is even, kept
    assert!(cache.cache_get(&3).is_none()); // 31 is odd, removed
    assert!(cache.cache_get(&4).is_some()); // 40 is even, kept
    assert_eq!(cache.cache_size(), 2);
}

#[cfg(feature = "time_stores")]
#[test]
fn test_ttl_sorted_cache_clone_cached() {
    use cached::stores::TtlSortedCache;
    use cached::{Cached, CloneCached};

    let mut cache = TtlSortedCache::<u32, u32>::new(Duration::from_secs(60));
    cache.cache_set(1, 100);

    // Existing, unexpired entry → (Some(v), false)
    let (val, expired) = cache.cache_get_with_expiry_status(&1u32);
    assert_eq!(val, Some(100));
    assert!(!expired);

    // Non-existent key → (None, false)
    let (val, expired) = cache.cache_get_with_expiry_status(&99u32);
    assert!(val.is_none());
    assert!(!expired);
}

#[cfg(all(feature = "time_stores", feature = "async_tokio_rt_multi_thread"))]
#[tokio::test]
async fn test_ttl_sorted_cache_cached_async() {
    use cached::stores::TtlSortedCache;
    use cached::CachedAsync;

    let mut cache = TtlSortedCache::<u32, u32>::new(Duration::from_secs(60));

    let val = CachedAsync::async_get_or_set_with(&mut cache, 1u32, || async { 42u32 }).await;
    assert_eq!(*val, 42);

    // Second call returns cached value.
    let val2 = CachedAsync::async_get_or_set_with(&mut cache, 1u32, || async { 99u32 }).await;
    assert_eq!(*val2, 42);
}

#[cfg(feature = "async_tokio_rt_multi_thread")]
#[tokio::test]
async fn test_expiring_lru_cache_cached_async() {
    use cached::CachedAsync;

    #[derive(Clone)]
    struct NeverExpires(u32);
    impl cached::Expires for NeverExpires {
        fn is_expired(&self) -> bool {
            false
        }
    }

    let mut cache = ExpiringLruCache::<u32, NeverExpires>::with_size(4);

    let val =
        CachedAsync::async_get_or_set_with(&mut cache, 1u32, || async { NeverExpires(42) }).await;
    assert_eq!(val.0, 42);

    // Cache hit: factory not called.
    let val2 =
        CachedAsync::async_get_or_set_with(&mut cache, 1u32, || async { NeverExpires(99) }).await;
    assert_eq!(val2.0, 42);

    assert_eq!(cache.cache_hits(), Some(1));
    assert_eq!(cache.cache_misses(), Some(1));
}

// ── Builder happy paths ────────────────────────────────────────────────────────

#[test]
fn test_lru_cache_builder_build() {
    use cached::Cached;
    let mut cache = LruCache::<u32, u32>::builder().size(4).build();
    cache.cache_set(1, 10);
    assert_eq!(cache.cache_get(&1), Some(&10));
    assert_eq!(cache.cache_capacity(), Some(4));
}

#[test]
fn test_unbound_cache_builder_build() {
    use cached::Cached;
    let mut cache = UnboundCache::<u32, u32>::builder().build();
    cache.cache_set(1, 10);
    assert_eq!(cache.cache_get(&1), Some(&10));
}

#[test]
fn test_expiring_lru_cache_builder_build() {
    use cached::Cached;
    #[derive(Clone)]
    struct AlwaysFresh(u32);
    impl Expires for AlwaysFresh {
        fn is_expired(&self) -> bool {
            false
        }
    }
    let mut cache = ExpiringLruCache::<u32, AlwaysFresh>::builder()
        .size(4)
        .build();
    cache.cache_set(1, AlwaysFresh(42));
    assert_eq!(cache.cache_get(&1).map(|v| v.0), Some(42));
}

#[test]
#[cfg(feature = "time_stores")]
fn test_ttl_cache_builder_build() {
    use cached::{Cached, TtlCache};
    let mut cache = TtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build();
    cache.cache_set(1, 10);
    assert_eq!(cache.cache_get(&1), Some(&10));
}

#[test]
#[cfg(feature = "time_stores")]
fn test_lru_ttl_cache_builder_build() {
    use cached::{Cached, LruTtlCache};
    let mut cache = LruTtlCache::<u32, u32>::builder()
        .size(4)
        .ttl(Duration::from_secs(60))
        .refresh(true)
        .build();
    cache.cache_set(1, 10);
    assert_eq!(cache.cache_get(&1), Some(&10));
    assert!(cache.refresh_on_hit());
}

#[test]
#[cfg(feature = "time_stores")]
fn test_ttl_sorted_cache_builder_build() {
    use cached::{stores::TtlSortedCache, Cached};
    let mut cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .size(4)
        .build();
    cache.cache_set(1, 10);
    assert_eq!(cache.cache_get(&1), Some(&10));
}

// ── `store()` getter ───────────────────────────────────────────────────────────

#[test]
#[cfg(feature = "time_stores")]
fn test_ttl_cache_store_getter() {
    use cached::{Cached, TtlCache};
    let mut cache = TtlCache::<u32, u32>::with_ttl(Duration::from_secs(60));
    cache.cache_set(1, 10);
    // store() gives direct access to the underlying HashMap<K, TimedEntry<V>>
    assert_eq!(cache.store().len(), 1);
    assert!(cache.store().contains_key(&1));
}

#[test]
fn test_unbound_cache_store_getter() {
    use cached::Cached;
    let mut cache = UnboundCache::<u32, u32>::new();
    cache.cache_set(1, 10);
    cache.cache_set(2, 20);
    assert_eq!(cache.store().len(), 2);
}

// ── `refresh_on_hit()` getter and `set_refresh_on_hit()` setter ──────────────

#[test]
#[cfg(feature = "time_stores")]
fn test_ttl_cache_refresh_getter_and_setter() {
    use cached::TtlCache;
    let mut cache = TtlCache::<u32, u32>::with_ttl_and_refresh(Duration::from_secs(60), false);
    assert!(!cache.refresh_on_hit());
    cache.set_refresh_on_hit(true);
    assert!(cache.refresh_on_hit());
    cache.set_refresh_on_hit(false);
    assert!(!cache.refresh_on_hit());
}

// ── CachedIter ────────────────────────────────────────────────────────────────

#[test]
fn test_cached_iter_unbound() {
    use cached::{Cached, CachedIter};
    let mut cache = UnboundCache::<u32, u32>::new();
    cache.cache_set(1, 10);
    cache.cache_set(2, 20);
    let mut pairs: Vec<_> = CachedIter::iter(&cache).collect();
    pairs.sort_by_key(|(k, _)| *k);
    assert_eq!(pairs, vec![(&1u32, &10u32), (&2u32, &20u32)]);
}

#[test]
fn test_cached_iter_lru() {
    use cached::{Cached, CachedIter};
    let mut cache = LruCache::<u32, u32>::with_size(4);
    cache.cache_set(1, 10);
    cache.cache_set(2, 20);
    let mut pairs: Vec<_> = CachedIter::iter(&cache).collect();
    pairs.sort_by_key(|(k, _)| *k);
    assert_eq!(pairs, vec![(&1u32, &10u32), (&2u32, &20u32)]);
}

#[test]
#[cfg(feature = "time_stores")]
fn test_cached_iter_ttl_cache() {
    use cached::{Cached, CachedIter, TtlCache};
    let mut cache = TtlCache::<u32, u32>::with_ttl(Duration::from_secs(60));
    cache.cache_set(1, 10);
    cache.cache_set(2, 20);
    let mut pairs: Vec<_> = CachedIter::iter(&cache).collect();
    pairs.sort_by_key(|(k, _)| *k);
    assert_eq!(pairs, vec![(&1u32, &10u32), (&2u32, &20u32)]);
}

#[test]
#[cfg(feature = "time_stores")]
fn test_cached_iter_ttl_cache_excludes_expired() {
    use cached::{Cached, CachedIter, TtlCache};
    let mut cache = TtlCache::<u32, u32>::with_ttl(Duration::from_millis(20));
    cache.cache_set(1, 10);
    cache.cache_set(2, 20);
    sleep(Duration::from_millis(40));
    // iter() on TtlCache filters out expired entries
    assert_eq!(CachedIter::iter(&cache).count(), 0);
}

#[test]
fn test_cached_iter_expiring_lru() {
    use cached::{Cached, CachedIter};
    #[derive(Clone)]
    struct Fresh;
    impl Expires for Fresh {
        fn is_expired(&self) -> bool {
            false
        }
    }
    let mut cache = ExpiringLruCache::<u32, Fresh>::with_size(4);
    cache.cache_set(1, Fresh);
    cache.cache_set(2, Fresh);
    let mut keys: Vec<_> = CachedIter::iter(&cache).map(|(k, _)| *k).collect();
    keys.sort();
    assert_eq!(keys, vec![1u32, 2u32]);
}

// ── CachedPeek on timed stores ────────────────────────────────────────────────

#[test]
#[cfg(feature = "time_stores")]
fn test_cached_peek_ttl_cache() {
    use cached::{Cached, CachedPeek, TtlCache};
    let mut cache = TtlCache::<u32, u32>::with_ttl(Duration::from_secs(60));
    cache.cache_set(1, 10);
    cache.cache_reset_metrics();

    // peek does not count as a hit
    assert_eq!(cache.cache_peek(&1), Some(&10));
    assert_eq!(cache.cache_hits(), Some(0));
    assert_eq!(cache.cache_misses(), Some(0));

    // peek on missing key also does not record a miss
    assert_eq!(cache.cache_peek(&99), None);
    assert_eq!(cache.cache_misses(), Some(0));
}

#[test]
#[cfg(feature = "time_stores")]
fn test_cached_peek_lru_ttl_cache() {
    use cached::{Cached, CachedPeek, LruTtlCache};
    let mut cache = LruTtlCache::<u32, u32>::with_size_and_ttl(4, Duration::from_secs(60));
    cache.cache_set(1, 10);
    cache.cache_reset_metrics();

    assert_eq!(cache.cache_peek(&1), Some(&10));
    assert_eq!(cache.cache_hits(), Some(0));
    assert_eq!(cache.cache_misses(), Some(0));
}

#[test]
#[cfg(feature = "time_stores")]
fn test_cached_peek_ttl_sorted_cache() {
    use cached::{stores::TtlSortedCache, Cached, CachedPeek};
    let mut cache = TtlSortedCache::<u32, u32>::new(Duration::from_secs(60));
    cache.cache_set(1, 10);
    cache.cache_reset_metrics();

    assert_eq!(cache.cache_peek(&1), Some(&10));
    assert_eq!(cache.cache_hits(), Some(0));
    assert_eq!(cache.cache_misses(), Some(0));
}

#[test]
fn test_cached_peek_lru_cache() {
    use cached::{Cached, CachedPeek};
    let mut cache = LruCache::<u32, u32>::with_size(4);
    cache.cache_set(1, 10);
    cache.cache_reset_metrics();

    assert_eq!(cache.cache_peek(&1), Some(&10));
    assert_eq!(cache.cache_hits(), Some(0));
    assert_eq!(cache.cache_misses(), Some(0));
}

#[test]
fn test_cached_peek_expiring_lru_cache() {
    use cached::{Cached, CachedPeek};
    #[derive(Clone)]
    struct AlwaysFresh(u32);
    impl Expires for AlwaysFresh {
        fn is_expired(&self) -> bool {
            false
        }
    }
    #[derive(Clone)]
    struct AlwaysExpired;
    impl Expires for AlwaysExpired {
        fn is_expired(&self) -> bool {
            true
        }
    }

    let mut cache = ExpiringLruCache::<u32, AlwaysFresh>::with_size(4);
    cache.cache_set(1, AlwaysFresh(10));
    cache.cache_reset_metrics();

    assert_eq!(cache.cache_peek(&1).map(|v| v.0), Some(10));
    assert_eq!(cache.cache_hits(), Some(0));
    assert_eq!(cache.cache_misses(), Some(0));

    // peek on a missing key does not record a miss
    assert!(cache.cache_peek(&99).is_none());
    assert_eq!(cache.cache_misses(), Some(0));

    // peek on a logically-expired entry returns None
    let mut cache2 = ExpiringLruCache::<u32, AlwaysExpired>::with_size(4);
    cache2.cache_set(1, AlwaysExpired);
    cache2.cache_reset_metrics();
    assert!(cache2.cache_peek(&1).is_none());
    assert_eq!(cache2.cache_hits(), Some(0));
    assert_eq!(cache2.cache_misses(), Some(0));
}

#[test]
fn test_cached_peek_unbound_cache() {
    use cached::{Cached, CachedPeek, UnboundCache};
    let mut cache = UnboundCache::<u32, u32>::new();
    cache.cache_set(1, 10);
    cache.cache_reset_metrics();

    assert_eq!(cache.cache_peek(&1), Some(&10));
    assert_eq!(cache.cache_hits(), Some(0));
    assert_eq!(cache.cache_misses(), Some(0));

    assert!(cache.cache_peek(&99).is_none());
    assert_eq!(cache.cache_misses(), Some(0));
}

#[test]
fn test_cached_peek_hashmap() {
    use cached::{Cached, CachedPeek};
    use std::collections::HashMap;
    let mut cache = HashMap::<u32, u32>::new();
    cache.cache_set(1, 10);

    // HashMap has no metrics; just confirm peek returns the right value
    assert_eq!(cache.cache_peek(&1), Some(&10));
    assert!(cache.cache_peek(&99).is_none());
    // Confirm peek does not evict
    assert_eq!(cache.cache_size(), 1);
}

#[test]
fn test_cached_read_hashmap() {
    use cached::{Cached, CachedRead};
    use std::collections::HashMap;
    let mut cache = HashMap::<u32, u32>::new();
    cache.cache_set(1, 42);

    // cache_get_read should return a shared reference without recording hits
    let hits_before = cache.cache_hits();
    assert_eq!(cache.cache_get_read(&1), Some(&42));
    assert_eq!(cache.cache_get_read(&99), None);
    // HashMap::cache_hits always returns None (no metrics), so hits are still equal
    assert_eq!(cache.cache_hits(), hits_before);
}

// ── CloneCached on more stores ────────────────────────────────────────────────

#[test]
#[cfg(feature = "time_stores")]
fn test_ttl_cache_clone_cached() {
    use cached::{Cached, CloneCached, TtlCache};
    let mut cache = TtlCache::<u32, u32>::with_ttl(Duration::from_secs(60));
    cache.cache_set(1, 100);

    let (val, expired) = cache.cache_get_with_expiry_status(&1u32);
    assert_eq!(val, Some(100));
    assert!(!expired);

    let (val, expired) = cache.cache_get_with_expiry_status(&99u32);
    assert!(val.is_none());
    assert!(!expired);
}

#[test]
fn test_expiring_lru_cache_clone_cached() {
    use cached::{Cached, CloneCached};
    #[derive(Clone, PartialEq, Debug)]
    struct Article {
        content: String,
        expired: bool,
    }
    impl Expires for Article {
        fn is_expired(&self) -> bool {
            self.expired
        }
    }

    let mut cache = ExpiringLruCache::<u32, Article>::with_size(4);
    cache.cache_set(
        1,
        Article {
            content: "hello".into(),
            expired: false,
        },
    );
    cache.cache_set(
        2,
        Article {
            content: "bye".into(),
            expired: true,
        },
    );

    // Non-expired entry: returns the value with expired=false
    let (val, is_exp) = cache.cache_get_with_expiry_status(&1u32);
    assert_eq!(val.as_ref().map(|a| a.content.as_str()), Some("hello"));
    assert!(!is_exp);

    // Expired entry: returns the value with expired=true
    let (val, is_exp) = cache.cache_get_with_expiry_status(&2u32);
    assert_eq!(val.as_ref().map(|a| a.content.as_str()), Some("bye"));
    assert!(is_exp);

    // Missing key
    let (val, is_exp) = cache.cache_get_with_expiry_status(&99u32);
    assert!(val.is_none());
    assert!(!is_exp);
}

// ── CacheEvict on timed stores ────────────────────────────────────────────────

#[test]
#[cfg(feature = "time_stores")]
fn test_cache_evict_ttl_cache() {
    use cached::{CacheEvict, Cached, TtlCache};
    let mut cache = TtlCache::<u32, u32>::with_ttl(Duration::from_millis(20));
    cache.cache_set(1, 10);
    cache.cache_set(2, 20);
    sleep(Duration::from_millis(40));
    let evicted = CacheEvict::evict(&mut cache);
    assert_eq!(evicted, 2);
    assert_eq!(cache.cache_size(), 0);
    assert_eq!(cache.cache_evictions(), Some(2));
}

#[test]
#[cfg(feature = "time_stores")]
fn test_cache_evict_lru_ttl_cache() {
    use cached::{CacheEvict, Cached, LruTtlCache};
    let mut cache = LruTtlCache::<u32, u32>::with_size_and_ttl(4, Duration::from_millis(20));
    cache.cache_set(1, 10);
    cache.cache_set(2, 20);
    sleep(Duration::from_millis(40));
    let evicted = CacheEvict::evict(&mut cache);
    assert_eq!(evicted, 2);
    assert_eq!(cache.cache_size(), 0);
}

// ── TtlSortedCache generic get<Q> integration ─────────────────────────────────

#[test]
#[cfg(feature = "time_stores")]
fn test_ttl_sorted_cache_generic_get_str_key() {
    use cached::stores::TtlSortedCache;
    let mut cache = TtlSortedCache::<String, u32>::new(Duration::from_secs(60));
    cache.insert(String::from("hello"), 1).unwrap();
    cache.insert(String::from("world"), 2).unwrap();

    // &str lookup works via Borrow<str>
    assert_eq!(cache.get("hello"), Some(&1));
    assert_eq!(cache.get("world"), Some(&2));
    assert_eq!(cache.get("missing"), None);
}

#[test]
#[cfg(feature = "time_stores")]
fn test_ttl_sorted_cache_generic_get_slice_key() {
    use cached::stores::TtlSortedCache;
    let mut cache = TtlSortedCache::<Vec<u32>, &str>::new(Duration::from_secs(60));
    cache.insert(vec![1, 2, 3], "abc").unwrap();

    // &[T] lookup works via Borrow<[T]>
    assert_eq!(cache.get([1u32, 2, 3].as_slice()), Some(&"abc"));
    assert_eq!(cache.get([9u32].as_slice()), None);
}

// ── Generic functions with `where` clauses ────────────────────────────────────
// Regression coverage: the macro-generated origin/`inner` helpers must carry
// the `where` clause. Quoting `#generics` alone drops it, so the bound below
// (`T: ToString`, used only in the helper body) would fail with E0599.
// For `#[cached]`/`#[concurrent_cached]` the cache is a `static`, so generics must be
// pinned out of the key/value via `key`+`convert` (and `ty`); `#[once]` has no
// such constraint since its static only holds the (concrete) value type.
#[cfg(feature = "proc_macro")]
mod generic_where_tests {
    use cached::macros::{cached, concurrent_cached, once};

    #[once]
    fn generic_once_where<T>(x: T) -> String
    where
        T: std::string::ToString,
    {
        x.to_string()
    }

    #[test]
    fn test_generic_once_where() {
        // `#[once]` caches the first produced value for all later calls.
        assert_eq!(generic_once_where(1u32), "1");
        assert_eq!(generic_once_where(2u32), "1");
    }

    #[cached(key = "String", convert = r#"{ x.to_string() }"#)]
    fn generic_cached_where<T>(x: T) -> String
    where
        T: std::string::ToString + Clone,
    {
        x.to_string()
    }

    #[test]
    fn test_generic_cached_where() {
        assert_eq!(generic_cached_where(7u32), "7");
        assert_eq!(generic_cached_where(7u32), "7");
        assert_eq!(generic_cached_where(8u64), "8");
    }

    // Minimal in-test `ConcurrentCached` store. Exercises the
    // `#[concurrent_cached]` where-clause/generics codegen without pulling in
    // Redis/Disk (and without a built-in in-memory `ConcurrentCached` store —
    // there isn't one by design; see the `Cached` vs `ConcurrentCached` split).
    struct TestStore {
        inner: std::sync::Mutex<std::collections::HashMap<String, String>>,
    }

    impl TestStore {
        fn new() -> Self {
            Self {
                inner: std::sync::Mutex::new(std::collections::HashMap::new()),
            }
        }
    }

    impl cached::ConcurrentCached<String, String> for TestStore {
        type Error = std::convert::Infallible;
        fn cache_get(&self, k: &String) -> Result<Option<String>, Self::Error> {
            Ok(self.inner.lock().unwrap().get(k).cloned())
        }
        fn cache_set(&self, k: String, v: String) -> Result<Option<String>, Self::Error> {
            Ok(self.inner.lock().unwrap().insert(k, v))
        }
        fn cache_remove(&self, k: &String) -> Result<Option<String>, Self::Error> {
            Ok(self.inner.lock().unwrap().remove(k))
        }
        fn set_refresh_on_hit(&mut self, _refresh: bool) -> bool {
            false
        }
    }

    #[concurrent_cached(
        ty = "TestStore",
        create = "{ TestStore::new() }",
        key = "String",
        convert = r#"{ x.to_string() }"#,
        map_error = r#"|e| e"#
    )]
    fn generic_concurrent_cached_where<T>(x: T) -> Result<String, std::convert::Infallible>
    where
        T: std::string::ToString,
    {
        Ok(x.to_string())
    }

    #[test]
    fn test_generic_concurrent_cached_where() {
        assert_eq!(generic_concurrent_cached_where(3u32).unwrap(), "3");
        assert_eq!(generic_concurrent_cached_where(3u32).unwrap(), "3");
        assert_eq!(generic_concurrent_cached_where(4u64).unwrap(), "4");
    }

    #[cfg(feature = "async")]
    mod async_generic {
        use cached::macros::cached;

        #[cached(key = "String", convert = r#"{ x.to_string() }"#)]
        async fn generic_cached_where_async<T>(x: T) -> String
        where
            T: std::string::ToString + Clone,
        {
            x.to_string()
        }

        #[tokio::test]
        async fn test_generic_cached_where_async() {
            assert_eq!(generic_cached_where_async(5u32).await, "5");
            assert_eq!(generic_cached_where_async(5u32).await, "5");
            assert_eq!(generic_cached_where_async(6u64).await, "6");
        }
    }
}

// ── CacheMetrics and hit_ratio ────────────────────────────────────────────────

#[test]
fn test_cache_metrics_and_hit_ratio() {
    use cached::Cached;

    // No lookups yet — hit_ratio should be None
    let mut cache = UnboundCache::<u32, u32>::new();
    let m = cache.metrics();
    assert_eq!(m.hits, Some(0));
    assert_eq!(m.misses, Some(0));
    assert_eq!(m.size, 0);
    assert!(m.capacity.is_none());
    assert!(m.hit_ratio().is_none(), "no lookups yet → None");

    cache.cache_set(1, 10);
    cache.cache_get(&1); // hit
    cache.cache_get(&2); // miss
    cache.cache_get(&1); // hit

    let m = cache.metrics();
    assert_eq!(m.hits, Some(2));
    assert_eq!(m.misses, Some(1));
    assert_eq!(m.size, 1);
    let ratio = m.hit_ratio().expect("should have ratio after lookups");
    assert!((ratio - 2.0 / 3.0).abs() < 1e-9);

    // LruCache: bounded, so capacity is Some
    let mut lru = LruCache::<u32, u32>::with_size(4);
    lru.cache_set(1, 10);
    lru.cache_get(&1);
    lru.cache_get(&99);
    let m = lru.metrics();
    assert_eq!(m.capacity, Some(4));
    assert_eq!(m.hits, Some(1));
    assert_eq!(m.misses, Some(1));
    let ratio = m.hit_ratio().unwrap();
    assert!((ratio - 0.5).abs() < 1e-9);
}

// ── TtlSortedCache::reserve and try_size_limit ────────────────────────────────

#[test]
#[cfg(feature = "time_stores")]
fn test_ttl_sorted_cache_reserve() {
    use cached::stores::TtlSortedCache;
    use cached::Cached;
    let mut cache = TtlSortedCache::<u32, u32>::new(Duration::from_secs(60));
    // reserve should not panic and the cache should still work normally
    cache.reserve(64);
    cache.cache_set(1, 10);
    assert_eq!(cache.cache_get(&1), Some(&10));
    assert_eq!(cache.cache_size(), 1);
}

#[test]
#[cfg(feature = "time_stores")]
fn test_ttl_sorted_cache_try_size_limit() {
    use cached::stores::TtlSortedCache;
    let mut cache = TtlSortedCache::<u32, u32>::new(Duration::from_secs(60));
    // Success: set a valid limit
    let prev = cache
        .try_size_limit(10)
        .expect("non-zero limit should succeed");
    assert!(prev.is_none(), "no previous limit");

    // Set another limit — returns old one
    let prev = cache.try_size_limit(20).unwrap();
    assert_eq!(prev, Some(10));

    // Error: size of zero is invalid
    let err = cache.try_size_limit(0);
    assert!(err.is_err(), "zero size limit must fail");
    assert_eq!(err.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
}

// ── result_fallback async ─────────────────────────────────────────────────────

#[cfg(all(
    feature = "proc_macro",
    feature = "time_stores",
    feature = "async_tokio_rt_multi_thread"
))]
mod result_fallback_async_tests {
    use super::sleep;
    use cached::time::Duration;

    #[cached::macros::cached(result = true, ttl = 1, result_fallback = true)]
    async fn async_always_failing() -> Result<String, ()> {
        Err(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_result_fallback_async() {
        use cached::Cached;
        // Prime the cache with a successful value
        ASYNC_ALWAYS_FAILING
            .write()
            .await
            .cache_set((), "hello".to_string());

        // Hits the cache — should get fallback value
        assert_eq!(async_always_failing().await, Ok("hello".to_string()));

        // Wait for TTL to expire
        sleep(Duration::from_millis(1100));

        // Even after expiry, result_fallback returns the last ok
        assert_eq!(async_always_failing().await, Ok("hello".to_string()));
    }
}

// ── CacheEvict on TtlSortedCache and ExpiringLruCache ────────────────────────

#[test]
#[cfg(feature = "time_stores")]
fn test_cache_evict_ttl_sorted_cache() {
    use cached::stores::TtlSortedCache;
    use cached::{CacheEvict, Cached};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_clone = evicted_count.clone();
    let mut cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_millis(20))
        .on_evict(move |_k: &u32, _v: &u32| {
            evicted_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build();

    cache.cache_set(1, 10);
    cache.cache_set(2, 20);
    cache.cache_set(3, 30);
    assert_eq!(cache.cache_size(), 3);

    sleep(Duration::from_millis(40));

    let evicted = CacheEvict::evict(&mut cache);
    assert_eq!(evicted, 3);
    assert_eq!(cache.cache_size(), 0);
    assert_eq!(cache.cache_evictions(), Some(3));
    assert_eq!(evicted_count.load(Ordering::Relaxed), 3);
}

#[test]
fn test_cache_evict_expiring_lru_cache() {
    use cached::{CacheEvict, Cached};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[derive(Clone)]
    struct Expirable {
        expired: bool,
    }
    impl cached::Expires for Expirable {
        fn is_expired(&self) -> bool {
            self.expired
        }
    }

    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_clone = evicted_count.clone();
    let mut cache = ExpiringLruCache::<u32, Expirable>::builder()
        .size(10)
        .on_evict(move |_k: &u32, _v: &Expirable| {
            evicted_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build();

    cache.cache_set(1, Expirable { expired: true });
    cache.cache_set(2, Expirable { expired: false });
    cache.cache_set(3, Expirable { expired: true });
    assert_eq!(cache.cache_size(), 3);

    let evicted = CacheEvict::evict(&mut cache);
    assert_eq!(evicted, 2);
    assert_eq!(cache.cache_size(), 1);
    assert_eq!(cache.cache_evictions(), Some(2));
    assert_eq!(evicted_count.load(Ordering::Relaxed), 2);
}

#[test]
fn test_expiring_lru_cache_get_does_not_inflate_inner_metrics() {
    use cached::Cached;

    #[derive(Clone)]
    struct Fresh;
    impl cached::Expires for Fresh {
        fn is_expired(&self) -> bool {
            false
        }
    }

    let mut cache = ExpiringLruCache::<u32, Fresh>::with_size(4);
    cache.cache_set(1, Fresh);
    cache.cache_reset_metrics();

    assert!(cache.cache_get(&1).is_some());
    assert_eq!(cache.cache_hits(), Some(1));
    assert_eq!(cache.cache_misses(), Some(0));
    // The inner LruCache counters must not be inflated by the outer cache_get.
    assert_eq!(cache.store().cache_hits(), Some(0));
    assert_eq!(cache.store().cache_misses(), Some(0));
}

#[test]
fn test_expiring_lru_cache_evictions_sum_lru_and_expiry() {
    use cached::Cached;

    #[derive(Clone)]
    struct Expirable {
        expired: bool,
    }
    impl cached::Expires for Expirable {
        fn is_expired(&self) -> bool {
            self.expired
        }
    }

    let mut cache = ExpiringLruCache::<u32, Expirable>::with_size(2);

    // Fill to capacity then insert a third entry: LRU evicts key 1 via the
    // inner LruCache's check_capacity path (inner store eviction counter = 1).
    cache.cache_set(1, Expirable { expired: false });
    cache.cache_set(2, Expirable { expired: false });
    cache.cache_set(3, Expirable { expired: false });
    assert_eq!(cache.store().cache_evictions(), Some(1)); // inner: 1 LRU eviction
    assert_eq!(cache.cache_evictions(), Some(1)); // sum so far

    // Mark key 2 as expired and access it to trigger an expiry eviction in the
    // outer ExpiringLruCache (outer eviction counter = 1).
    cache.cache_set(2, Expirable { expired: true });
    assert!(cache.cache_get(&2).is_none());
    // total = 1 (LRU capacity, inner) + 1 (expiry, outer) = 2
    assert_eq!(cache.cache_evictions(), Some(2));
}

// Maximal-pairwise positive coverage for macro arguments not exercised by the
// tests above. Each case pins a previously-uncovered argument and pairs it
// with an interacting argument, asserting real caching behavior (not merely
// that expansion compiles). Metrics are introspected where the access pattern
// is unambiguous; otherwise a call counter proves the body ran exactly once.
#[cfg(feature = "proc_macro")]
mod macro_arg_pairwise {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // name + unbound: custom static identifier, explicit unbound store.
    // Default sync_lock is RwLock, so the named static is read via `.write()`.
    #[cached(name = "PAIRWISE_NAMED_UNBOUND", unbound)]
    fn named_unbound(n: u32) -> u32 {
        n + 1
    }

    #[test]
    fn test_name_with_unbound() {
        assert_eq!(named_unbound(2), 3);
        assert_eq!(named_unbound(2), 3);
        let cache = PAIRWISE_NAMED_UNBOUND.write();
        assert_eq!(cache.cache_hits(), Some(1));
        assert_eq!(cache.cache_misses(), Some(1));
    }

    // size + sync_lock = "mutex": LruCache behind a Mutex, read via `.lock()`.
    #[cached(size = 2, sync_lock = "mutex")]
    fn sized_mutex(n: u32) -> u32 {
        n * 2
    }

    #[test]
    fn test_size_with_sync_lock_mutex() {
        assert_eq!(sized_mutex(3), 6);
        assert_eq!(sized_mutex(3), 6);
        let cache = SIZED_MUTEX.lock();
        assert_eq!(cache.cache_hits(), Some(1));
        assert_eq!(cache.cache_misses(), Some(1));
    }

    // unsync_reads + sync_lock = "rwlock": positive counterpart of the
    // `unsync_reads + mutex` compile-fail. The unsync read path returns the
    // cached value without re-running the body.
    static UNSYNC_CALLS: AtomicUsize = AtomicUsize::new(0);

    // Documented spelling `"rwlock"` (now accepted alongside darling's
    // snake_case) paired with unsync_reads.
    #[cached(unsync_reads = true, sync_lock = "rwlock")]
    fn unsync_rwlock(n: u32) -> u32 {
        UNSYNC_CALLS.fetch_add(1, Ordering::SeqCst);
        n + 100
    }

    #[test]
    fn test_unsync_reads_with_sync_lock_rwlock() {
        assert_eq!(unsync_rwlock(1), 101);
        assert_eq!(unsync_rwlock(1), 101);
        assert_eq!(unsync_rwlock(1), 101);
        assert_eq!(UNSYNC_CALLS.load(Ordering::SeqCst), 1);
    }

    // Both `sync_lock` spellings must select the RwLock-backed static; assert
    // that by reading each named static via the RwLock-only `.write()` API.
    #[cached(name = "SYNC_LOCK_DOC_SPELLING", sync_lock = "rwlock")]
    fn sync_lock_doc(n: u32) -> u32 {
        n + 1
    }

    #[cached(name = "SYNC_LOCK_SNAKE_SPELLING", sync_lock = "rw_lock")]
    fn sync_lock_snake(n: u32) -> u32 {
        n + 1
    }

    #[test]
    fn test_sync_lock_both_spellings_select_rwlock() {
        assert_eq!(sync_lock_doc(1), 2);
        assert_eq!(sync_lock_snake(1), 2);
        // `.write()` only exists on the RwLock wrapper; compiling+passing here
        // proves both spellings resolved to RwLock.
        assert_eq!(SYNC_LOCK_DOC_SPELLING.write().cache_misses(), Some(1));
        assert_eq!(SYNC_LOCK_SNAKE_SPELLING.write().cache_misses(), Some(1));
    }

    // sync_writes = "by_key" + explicit non-default sync_writes_buckets.
    static BY_KEY_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[cached(sync_writes = "by_key", sync_writes_buckets = 4)]
    fn by_key_buckets(n: u32) -> u32 {
        BY_KEY_CALLS.fetch_add(1, Ordering::SeqCst);
        n + 7
    }

    #[test]
    fn test_by_key_with_sync_writes_buckets() {
        assert_eq!(by_key_buckets(5), 12);
        assert_eq!(by_key_buckets(5), 12);
        assert_eq!(by_key_buckets(5), 12);
        assert_eq!(BY_KEY_CALLS.load(Ordering::SeqCst), 1);
    }

    // ---- #[once] argument gaps ----

    // once + with_cached_flag (alone): flagged value reports cache state, and
    // `#[once]` retains the first value for every subsequent argument.
    #[once(with_cached_flag = true)]
    fn once_flag(n: u32) -> cached::Return<u32> {
        cached::Return::new(n + 1)
    }

    #[test]
    fn test_once_with_cached_flag() {
        let first = once_flag(10);
        assert!(!first.was_cached);
        assert_eq!(*first, 11);
        let second = once_flag(999);
        assert!(second.was_cached);
        assert_eq!(*second, 11);
    }

    // once + result + with_cached_flag (pairwise).
    #[once(result = true, with_cached_flag = true)]
    fn once_result_flag(ok: bool) -> Result<cached::Return<u32>, ()> {
        if ok {
            Ok(cached::Return::new(1))
        } else {
            Err(())
        }
    }

    #[test]
    fn test_once_result_with_cached_flag() {
        assert!(once_result_flag(false).is_err());
        let ok = once_result_flag(true).unwrap();
        assert!(!ok.was_cached);
        let cached_ok = once_result_flag(true).unwrap();
        assert!(cached_ok.was_cached);
        assert_eq!(*cached_ok, 1);
    }

    // once + option + with_cached_flag (pairwise).
    #[once(option = true, with_cached_flag = true)]
    fn once_option_flag(some: bool) -> Option<cached::Return<u32>> {
        if some {
            Some(cached::Return::new(2))
        } else {
            None
        }
    }

    #[test]
    fn test_once_option_with_cached_flag() {
        assert!(once_option_flag(false).is_none());
        let s = once_option_flag(true).unwrap();
        assert!(!s.was_cached);
        let c = once_option_flag(true).unwrap();
        assert!(c.was_cached);
        assert_eq!(*c, 2);
    }

    // once + name + ttl (pairwise; the TTL store requires `time_stores`).
    #[cfg(feature = "time_stores")]
    #[once(name = "PAIRWISE_ONCE_NAMED_TTL", ttl = 100)]
    fn once_named_ttl(n: u32) -> u32 {
        n + 3
    }

    #[cfg(feature = "time_stores")]
    #[test]
    fn test_once_name_with_ttl() {
        assert_eq!(once_named_ttl(4), 7);
        assert_eq!(once_named_ttl(123), 7);
        let cache = PAIRWISE_ONCE_NAMED_TTL.read();
        assert!(cache.is_some());
    }
}

#[cfg(all(feature = "time_stores", feature = "async"))]
mod async_cache_store_tests {
    use cached::time::Duration;
    use cached::Expires;
    use cached::{
        CachedAsync, ExpiringLruCache, LruTtlCache, TtlCache, TtlSortedCache, UnboundCache,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_ttl_cache_async() {
        let mut cache = TtlCache::builder().ttl(Duration::from_millis(50)).build();

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let val = cache
            .async_get_or_set_with(1, || {
                let calls = calls_clone.clone();
                async move {
                    calls.fetch_add(1, Ordering::Relaxed);
                    "hello".to_string()
                }
            })
            .await;
        assert_eq!(val, "hello");
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        // Get from cache
        let val = cache
            .async_get_or_set_with(1, || async {
                calls.fetch_add(1, Ordering::Relaxed);
                "world".to_string()
            })
            .await;
        assert_eq!(val, "hello");
        assert_eq!(calls.load(Ordering::Relaxed), 1);

        // Wait for expiration
        tokio::time::sleep(tokio::time::Duration::from_millis(60)).await;

        let val = cache
            .async_get_or_set_with(1, || async {
                calls.fetch_add(1, Ordering::Relaxed);
                "world".to_string()
            })
            .await;
        assert_eq!(val, "world");
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn test_ttl_cache_async_try_evict() {
        let evicted = Arc::new(AtomicUsize::new(0));
        let evicted_clone = evicted.clone();
        let mut cache = TtlCache::builder()
            .ttl(Duration::from_millis(50))
            .on_evict(move |_, _| {
                evicted_clone.fetch_add(1, Ordering::Relaxed);
            })
            .build();

        let val = cache
            .async_try_get_or_set_with(1, || async { Ok::<_, ()>("hello".to_string()) })
            .await
            .unwrap();
        assert_eq!(val, "hello");
        assert_eq!(evicted.load(Ordering::Relaxed), 0);

        // Wait for expiration
        tokio::time::sleep(tokio::time::Duration::from_millis(60)).await;

        // Try get or set with a new value, triggers evict on old expired value
        let val = cache
            .async_try_get_or_set_with(1, || async { Ok::<_, ()>("world".to_string()) })
            .await
            .unwrap();
        assert_eq!(val, "world");
        assert_eq!(evicted.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn test_lru_ttl_cache_async() {
        let evicted = Arc::new(AtomicUsize::new(0));
        let evicted_clone = evicted.clone();
        let mut cache = LruTtlCache::builder()
            .size(2)
            .ttl(Duration::from_millis(50))
            .on_evict(move |_, _| {
                evicted_clone.fetch_add(1, Ordering::Relaxed);
            })
            .build();

        cache
            .async_get_or_set_with(1, || async { "one".to_string() })
            .await;
        cache
            .async_get_or_set_with(2, || async { "two".to_string() })
            .await;
        assert_eq!(evicted.load(Ordering::Relaxed), 0);

        // Trigger LRU eviction by size limit
        cache
            .async_get_or_set_with(3, || async { "three".to_string() })
            .await;
        assert_eq!(evicted.load(Ordering::Relaxed), 1);

        // Wait for expiration
        tokio::time::sleep(tokio::time::Duration::from_millis(60)).await;

        // Trigger evict on expired value
        cache
            .async_get_or_set_with(3, || async { "new_three".to_string() })
            .await;
        assert_eq!(evicted.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn test_ttl_sorted_cache_async() {
        let evicted = Arc::new(AtomicUsize::new(0));
        let evicted_clone = evicted.clone();
        let mut cache = TtlSortedCache::builder()
            .ttl(Duration::from_millis(50))
            .on_evict(move |_, _| {
                evicted_clone.fetch_add(1, Ordering::Relaxed);
            })
            .build();

        cache
            .async_get_or_set_with(1, || async { "one".to_string() })
            .await;
        assert_eq!(evicted.load(Ordering::Relaxed), 0);

        tokio::time::sleep(tokio::time::Duration::from_millis(60)).await;

        cache
            .async_get_or_set_with(1, || async { "new_one".to_string() })
            .await;
        assert_eq!(evicted.load(Ordering::Relaxed), 1);
    }

    #[derive(Clone)]
    struct ExpiringVal {
        expired: bool,
    }
    impl Expires for ExpiringVal {
        fn is_expired(&self) -> bool {
            self.expired
        }
    }

    #[tokio::test]
    async fn test_expiring_lru_cache_async() {
        let evicted = Arc::new(AtomicUsize::new(0));
        let evicted_clone = evicted.clone();
        let mut cache = ExpiringLruCache::builder()
            .size(2)
            .on_evict(move |_, _| {
                evicted_clone.fetch_add(1, Ordering::Relaxed);
            })
            .build();

        cache
            .async_get_or_set_with(1, || async { ExpiringVal { expired: true } })
            .await;
        assert_eq!(evicted.load(Ordering::Relaxed), 0);

        // Fetching it when expired triggers eviction
        cache
            .async_get_or_set_with(1, || async { ExpiringVal { expired: false } })
            .await;
        assert_eq!(evicted.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn test_unbound_cache_async() {
        let mut cache = UnboundCache::new();
        let val = cache
            .async_get_or_set_with(1, || async { "hello".to_string() })
            .await;
        assert_eq!(val, "hello");
    }
}
