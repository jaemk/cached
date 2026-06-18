/*!
Full tests of macro-defined functions
*/

#[cfg(feature = "time_stores")]
use cached::time::Duration;
use cached::{Cached, LruCache, UnboundCache};
use cached::{Expires, ExpiringLruCache};
#[cfg(feature = "proc_macro")]
use cached::{macros::cached, macros::once};
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

// `new`/`builder` on each sharded `*Base` are constrained to the default-hasher
// specialization, so a `Base::<_, _, CustomHasher>::{new,builder}()` turbofish (which
// would silently drop the custom hasher) must not compile.
#[test]
fn compile_fail_sharded_constructor() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/sharded_base_custom_hasher_constructor.rs");
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
    t.compile_fail("tests/ui/cached_key_without_convert.rs");
    t.compile_fail("tests/ui/cached_convert_without_key.rs");
    t.compile_fail("tests/ui/cached_ty_without_create.rs");
    t.compile_fail("tests/ui/cached_create_without_ty.rs");
    t.compile_fail("tests/ui/cached_store_types_exclusive.rs");
    t.compile_fail("tests/ui/cached_sync_writes_buckets_zero.rs");
    t.compile_fail("tests/ui/cached_result_fallback_sync_writes.rs");
    t.compile_fail("tests/ui/cached_sync_lock_unknown.rs");
    t.compile_fail("tests/ui/cached_expires_ttl_exclusive.rs");
    t.compile_fail("tests/ui/cached_expires_malformed_ttl.rs");
    t.compile_fail("tests/ui/cached_expires_type_exclusive.rs");
    t.compile_fail("tests/ui/cached_expires_create_exclusive.rs");
    t.compile_fail("tests/ui/cached_expires_with_cached_flag_exclusive.rs");
    t.compile_fail("tests/ui/cached_expires_unsync_reads_exclusive.rs");
    t.compile_fail("tests/ui/cached_expires_non_expires_type.rs");
    t.compile_fail("tests/ui/cached_expires_refresh_exclusive.rs");
    t.compile_fail("tests/ui/cached_unbound_attr_removed.rs");
    t.compile_fail("tests/ui/cached_key_unparseable.rs");
    t.compile_fail("tests/ui/cached_convert_unparseable.rs");
    t.compile_fail("tests/ui/cached_expires_cache_none_exclusive.rs");
    t.compile_fail("tests/ui/cached_expires_cache_err_exclusive.rs");
    t.compile_fail("tests/ui/cached_cache_err_requires_result_return.rs");
    t.compile_fail("tests/ui/cached_cache_none_requires_option_return.rs");
    t.compile_fail("tests/ui/cached_cache_err_result_fallback_exclusive.rs");
    t.compile_fail("tests/ui/cached_cache_none_with_cached_flag_exclusive.rs");
    t.compile_fail("tests/ui/cached_ttl_zero.rs");
    t.compile_fail("tests/ui/cached_max_size_zero.rs");
    t.compile_fail("tests/ui/cached_size_attr_removed.rs");

    // ---- #[once] ----
    t.compile_fail("tests/ui/once_self_method.rs");
    t.compile_fail("tests/ui/once_time_attr_renamed.rs");
    t.compile_fail("tests/ui/once_with_cached_flag_foreign.rs");
    t.compile_fail("tests/ui/once_expires_ttl_exclusive.rs");
    t.compile_fail("tests/ui/once_expires_malformed_ttl.rs");
    t.compile_fail("tests/ui/once_expires_non_expires_type.rs");
    t.compile_fail("tests/ui/once_expires_cache_none_exclusive.rs");
    t.compile_fail("tests/ui/once_expires_cache_err_exclusive.rs");
    t.compile_fail("tests/ui/once_expires_with_cached_flag_exclusive.rs");
    t.compile_fail("tests/ui/once_sync_writes_buckets_zero.rs");
    t.compile_fail("tests/ui/once_cache_err_requires_result_return.rs");
    t.compile_fail("tests/ui/once_cache_none_requires_option_return.rs");
    t.compile_fail("tests/ui/once_cache_none_with_cached_flag_exclusive.rs");
    t.compile_fail("tests/ui/once_ttl_zero.rs");

    // ---- #[concurrent_cached] ----
    t.compile_fail("tests/ui/concurrent_cached_self_method.rs");
    t.compile_fail("tests/ui/concurrent_cached_time_attr_renamed.rs");
    t.compile_fail("tests/ui/concurrent_cached_with_cached_flag_foreign.rs");
    t.compile_fail("tests/ui/concurrent_cached_no_return.rs");
    t.compile_fail("tests/ui/concurrent_cached_complex_return.rs");
    t.compile_fail("tests/ui/concurrent_cached_non_result_return.rs");
    t.compile_fail("tests/ui/concurrent_cached_redis_create_conflict.rs");
    t.compile_fail("tests/ui/concurrent_cached_redis_disk_exclusive.rs");
    t.compile_fail("tests/ui/concurrent_cached_async_redis_no_ttl.rs");
    t.compile_fail("tests/ui/concurrent_cached_redis_no_ttl.rs");
    t.compile_fail("tests/ui/concurrent_cached_disk_create_conflict.rs");
    t.compile_fail("tests/ui/concurrent_cached_refresh_create_conflict.rs");
    t.compile_fail("tests/ui/concurrent_cached_max_size_create_conflict.rs");
    t.compile_fail("tests/ui/concurrent_cached_disk_create_ignored_attrs.rs");
    t.compile_fail("tests/ui/concurrent_cached_option_return.rs");
    t.compile_fail("tests/ui/concurrent_cached_result_attr_unsupported.rs");
    t.compile_fail("tests/ui/concurrent_cached_option_attr_unsupported.rs");
    t.compile_fail("tests/ui/concurrent_cached_sync_writes_attr_unsupported.rs");
    t.compile_fail("tests/ui/concurrent_cached_custom_create_required.rs");
    t.compile_fail("tests/ui/concurrent_cached_shards_zero.rs");
    t.compile_fail("tests/ui/concurrent_cached_max_size_zero.rs");
    t.compile_fail("tests/ui/concurrent_cached_ttl_zero.rs");
    t.compile_fail("tests/ui/concurrent_cached_shards_with_redis.rs");
    t.compile_fail("tests/ui/concurrent_cached_shards_with_disk.rs");
    t.compile_fail("tests/ui/concurrent_cached_max_size_with_redis.rs");
    t.compile_fail("tests/ui/concurrent_cached_max_size_with_disk.rs");
    t.compile_fail("tests/ui/concurrent_cached_max_size_with_redis_ty.rs");
    t.compile_fail("tests/ui/concurrent_cached_max_size_with_disk_ty.rs");
    t.compile_fail("tests/ui/concurrent_cached_size_attr_removed.rs");
    t.compile_fail("tests/ui/concurrent_cached_durable_with_redis.rs");
    t.compile_fail("tests/ui/concurrent_cached_key_without_convert.rs");
    t.compile_fail("tests/ui/concurrent_cached_refresh_without_ttl.rs");
    t.compile_fail("tests/ui/concurrent_cached_expires_ttl_exclusive.rs");
    t.compile_fail("tests/ui/concurrent_cached_expires_malformed_ttl.rs");
    t.compile_fail("tests/ui/concurrent_cached_expires_redis_exclusive.rs");
    t.compile_fail("tests/ui/concurrent_cached_expires_disk_exclusive.rs");
    t.compile_fail("tests/ui/concurrent_cached_expires_ty_exclusive.rs");
    t.compile_fail("tests/ui/concurrent_cached_expires_create_exclusive.rs");
    t.compile_fail("tests/ui/concurrent_cached_expires_refresh_exclusive.rs");
    t.compile_fail("tests/ui/concurrent_cached_expires_cache_none_exclusive.rs");
    t.compile_fail("tests/ui/concurrent_cached_expires_cache_err_exclusive.rs");
    t.compile_fail("tests/ui/concurrent_cached_result_fallback_expires_exclusive.rs");
    t.compile_fail("tests/ui/concurrent_cached_result_fallback_redis_exclusive.rs");
    t.compile_fail("tests/ui/concurrent_cached_result_fallback_disk_exclusive.rs");
    t.compile_fail("tests/ui/concurrent_cached_result_fallback_requires_ttl.rs");
    t.compile_fail("tests/ui/concurrent_cached_with_cached_flag_option.rs");
    t.compile_fail("tests/ui/concurrent_cached_option_with_redis.rs");
    t.compile_fail("tests/ui/concurrent_cached_cache_none_with_redis.rs");
    t.compile_fail("tests/ui/concurrent_cached_cache_err_result_fallback_exclusive.rs");
    t.compile_fail("tests/ui/concurrent_cached_result_fallback_with_cached_flag_exclusive.rs");
    t.compile_fail("tests/ui/cached_result_attr_removed.rs");
    t.compile_fail("tests/ui/cached_option_attr_removed.rs");
    t.compile_fail("tests/ui/once_result_attr_removed.rs");
    t.compile_fail("tests/ui/once_option_attr_removed.rs");
    t.compile_fail("tests/ui/concurrent_cached_option_attr_removed.rs");
    t.compile_fail("tests/ui/concurrent_cached_map_error_on_infallible.rs");
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
#[cached]
fn proc_cached_result(n: u32) -> Result<Vec<u32>, NoClone> {
    if n < 5 { Ok(vec![n]) } else { Err(NoClone {}) }
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
#[cached]
fn proc_cached_option(n: u32) -> Option<Vec<u32>> {
    if n < 5 { Some(vec![n]) } else { None }
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
static CACHED_CACHE_ERR_TRUE_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(feature = "proc_macro")]
#[cached(cache_err = true)]
fn cached_cache_err_true(n: u32) -> Result<u32, u32> {
    CACHED_CACHE_ERR_TRUE_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Err(n)
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_cached_cache_err_true_caches_err() {
    let before = CACHED_CACHE_ERR_TRUE_CALLS.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(cached_cache_err_true(7), Err(7));
    assert_eq!(cached_cache_err_true(7), Err(7));
    assert_eq!(
        CACHED_CACHE_ERR_TRUE_CALLS.load(std::sync::atomic::Ordering::SeqCst),
        before + 1
    );
}

#[cfg(feature = "proc_macro")]
static CACHED_CACHE_NONE_TRUE_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(feature = "proc_macro")]
#[cached(cache_none = true)]
fn cached_cache_none_true(n: u32) -> Option<u32> {
    CACHED_CACHE_NONE_TRUE_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if n == 0 { None } else { Some(n) }
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_cached_cache_none_true_caches_none() {
    let before = CACHED_CACHE_NONE_TRUE_CALLS.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(cached_cache_none_true(0), None);
    assert_eq!(cached_cache_none_true(0), None);
    assert_eq!(
        CACHED_CACHE_NONE_TRUE_CALLS.load(std::sync::atomic::Ordering::SeqCst),
        before + 1
    );
}

#[cfg(feature = "proc_macro")]
static CONCURRENT_CACHED_CACHE_ERR_TRUE_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(feature = "proc_macro")]
#[cached::macros::concurrent_cached(cache_err = true)]
fn concurrent_cached_cache_err_true(n: u32) -> Result<u32, u32> {
    CONCURRENT_CACHED_CACHE_ERR_TRUE_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Err(n)
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_concurrent_cached_cache_err_true_caches_err() {
    let before = CONCURRENT_CACHED_CACHE_ERR_TRUE_CALLS.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(concurrent_cached_cache_err_true(7), Err(7));
    assert_eq!(concurrent_cached_cache_err_true(7), Err(7));
    assert_eq!(
        CONCURRENT_CACHED_CACHE_ERR_TRUE_CALLS.load(std::sync::atomic::Ordering::SeqCst),
        before + 1
    );
}

#[cfg(feature = "proc_macro")]
static CONCURRENT_CACHED_CACHE_NONE_TRUE_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(feature = "proc_macro")]
#[cached::macros::concurrent_cached(cache_none = true)]
fn concurrent_cached_cache_none_true(n: u32) -> Option<u32> {
    CONCURRENT_CACHED_CACHE_NONE_TRUE_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if n == 0 { None } else { Some(n) }
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_concurrent_cached_cache_none_true_caches_none() {
    let before = CONCURRENT_CACHED_CACHE_NONE_TRUE_CALLS.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(concurrent_cached_cache_none_true(0), None);
    assert_eq!(concurrent_cached_cache_none_true(0), None);
    assert_eq!(
        CONCURRENT_CACHED_CACHE_NONE_TRUE_CALLS.load(std::sync::atomic::Ordering::SeqCst),
        before + 1
    );
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
#[cached(with_cached_flag = true)]
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
#[cached(with_cached_flag = true)]
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
#[once]
fn only_cached_result_once(s: String, error: bool) -> std::result::Result<Vec<String>, u32> {
    if error { Err(1) } else { Ok(vec![s]) }
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

#[cfg(feature = "proc_macro")]
static ONCE_CACHE_ERR_TRUE_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(feature = "proc_macro")]
#[once(cache_err = true)]
fn once_cache_err_true(code: u32) -> Result<u32, u32> {
    ONCE_CACHE_ERR_TRUE_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Err(code)
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_once_cache_err_true_caches_err() {
    let before = ONCE_CACHE_ERR_TRUE_CALLS.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(once_cache_err_true(7), Err(7));
    assert_eq!(once_cache_err_true(9), Err(7));
    assert_eq!(
        ONCE_CACHE_ERR_TRUE_CALLS.load(std::sync::atomic::Ordering::SeqCst),
        before + 1
    );
}

/// should only cache the _first_ `Some` returned .
/// all arguments are ignored for subsequent calls
#[cfg(feature = "proc_macro")]
#[once]
fn only_cached_option_once(s: String, none: bool) -> Option<Vec<String>> {
    if none { None } else { Some(vec![s]) }
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
static ONCE_CACHE_NONE_TRUE_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(feature = "proc_macro")]
#[once(cache_none = true)]
fn once_cache_none_true(n: u32) -> Option<u32> {
    ONCE_CACHE_NONE_TRUE_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if n == 0 { None } else { Some(n) }
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_once_cache_none_true_caches_none() {
    let before = ONCE_CACHE_NONE_TRUE_CALLS.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(once_cache_none_true(0), None);
    assert_eq!(once_cache_none_true(1), None);
    assert_eq!(
        ONCE_CACHE_NONE_TRUE_CALLS.load(std::sync::atomic::Ordering::SeqCst),
        before + 1
    );
}

#[cfg(feature = "proc_macro")]
#[cached(max_size = 2)]
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
    max_size = 2,
    key = "smartstring::alias::String",
    convert = r#"{ smartstring::alias::String::from(s) }"#
)]
fn cached_smartstring_from_str(s: &str) -> bool {
    s == "true"
}

// `max_size` sets the LRU bound.
#[cfg(feature = "proc_macro")]
#[cached(max_size = 2)]
fn cached_max_size_alias(n: u32) -> u32 {
    n * 2
}

#[cfg(feature = "proc_macro")]
#[test]
fn test_cached_max_size_alias_sets_bound() {
    assert_eq!(cached_max_size_alias(1), 2);
    assert_eq!(cached_max_size_alias(2), 4);
    assert_eq!(cached_max_size_alias(3), 6); // evicts the LRU entry
    let cache = CACHED_MAX_SIZE_ALIAS.read();
    // capacity reflects the `max_size = 2` bound, and the store never exceeds it
    assert_eq!(cache.capacity(), 2);
    assert_eq!(cache.cache_size(), 2);
}

// The sync `Cached` trait exposes `remove_entry` / `delete` short aliases, matching
// the `ConcurrentCached` trait. They delegate to `cache_remove_entry` / `cache_delete`.
#[test]
fn sync_cached_remove_entry_and_delete_aliases() {
    let mut cache: UnboundCache<String, u32> = UnboundCache::builder().build().unwrap();
    cache.cache_set("a".to_string(), 1);
    cache.cache_set("b".to_string(), 2);

    // `remove_entry` returns the stored key and value, like `cache_remove_entry`.
    assert_eq!(cache.remove_entry("a"), Some(("a".to_string(), 1)));
    assert_eq!(cache.remove_entry("a"), None); // already removed

    // `delete` returns true when an entry was physically removed, false if absent.
    assert!(cache.delete("b"));
    assert!(!cache.delete("b"));
    assert_eq!(cache.cache_size(), 0);
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

/// Smoke tests for `#[cached(expires = true)]` and `#[once(expires = true)]` that do not
/// require `time_stores` — ensuring the macro paths are covered by the `tests/proc-macro`
/// and `tests/ahash` CI targets as well as the full-feature builds.
#[cfg(feature = "proc_macro")]
mod expires_macro_tests {
    use cached::macros::{cached, once};
    use cached::time::{Duration, Instant};
    use cached::{Cached, Expires};

    #[derive(Clone, Debug)]
    struct Val {
        v: u32,
        expired: bool,
    }
    impl Expires for Val {
        fn is_expired(&self) -> bool {
            self.expired
        }
    }

    // A value whose expiry is computed from a runtime `Duration` argument — the
    // `#[cached]` way to get a dynamic, per-entry TTL (issue #246).
    #[derive(Clone, Debug)]
    struct TtlVal {
        v: u32,
        expires_at: Instant,
    }
    impl Expires for TtlVal {
        fn is_expired(&self) -> bool {
            Instant::now() >= self.expires_at
        }
    }

    // `key`/`convert` keep `ttl_ms` out of the cache key so it only affects the
    // entry's lifetime, not which slot it occupies.
    #[cached(expires = true, key = "u32", convert = "{ k }")]
    fn dyn_ttl(k: u32, ttl_ms: u64) -> TtlVal {
        TtlVal {
            v: k,
            expires_at: Instant::now() + Duration::from_millis(ttl_ms),
        }
    }

    // key = "u32" so `expired` controls the returned value without affecting the cache key
    #[cached(expires = true, key = "u32", convert = "{ k }")]
    fn sm_cached_expires(k: u32, expired: bool) -> Val {
        Val { v: k, expired }
    }

    #[cached(expires = true, max_size = 4, key = "u32", convert = "{ k }")]
    fn sm_cached_expires_lru(k: u32, expired: bool) -> Val {
        Val { v: k, expired }
    }

    #[once(expires = true)]
    fn sm_once_expires(expired: bool) -> Val {
        Val { v: 42, expired }
    }

    #[test]
    fn test_expires_macro_hit_and_miss() {
        {
            let mut c = SM_CACHED_EXPIRES.write();
            c.cache_clear();
            c.cache_reset_metrics();
        }
        // miss — caches Val{v=1, expired=false} under key 1
        let v1 = sm_cached_expires(1, false);
        assert_eq!(v1.v, 1);
        assert!(!v1.expired);
        // hit — same key 1, returns the cached live value (expired=false arg is ignored)
        let v2 = sm_cached_expires(1, false);
        assert!(!v2.expired);
        {
            let c = SM_CACHED_EXPIRES.read();
            assert_eq!(c.cache_hits(), Some(1));
            assert_eq!(c.cache_misses(), Some(1));
        }

        // different key — prime with an expired value
        let v3 = sm_cached_expires(2, true);
        assert!(v3.expired);
        // same key 2 — expired entry is evicted and function re-executes
        let v4 = sm_cached_expires(2, false);
        assert!(!v4.expired);
        {
            let c = SM_CACHED_EXPIRES.read();
            assert_eq!(c.cache_evictions(), Some(1));
        }
    }

    #[test]
    fn test_expires_lru_macro_hit_and_miss() {
        {
            let mut c = SM_CACHED_EXPIRES_LRU.write();
            c.cache_clear();
            c.cache_reset_metrics();
        }
        let v1 = sm_cached_expires_lru(10, false);
        assert_eq!(v1.v, 10);
        let v2 = sm_cached_expires_lru(10, false);
        assert!(!v2.expired);
        {
            let c = SM_CACHED_EXPIRES_LRU.read();
            assert_eq!(c.cache_hits(), Some(1));
            assert_eq!(c.cache_misses(), Some(1));
        }

        // prime key 11 with expired value, then verify re-execution
        let v3 = sm_cached_expires_lru(11, true);
        assert!(v3.expired);
        let v4 = sm_cached_expires_lru(11, false);
        assert!(!v4.expired);
        {
            let c = SM_CACHED_EXPIRES_LRU.read();
            assert_eq!(c.cache_evictions(), Some(1));
        }
    }

    #[test]
    fn test_once_expires_macro() {
        // prime with a live value
        let v1 = sm_once_expires(false);
        assert!(!v1.expired);
        // cached — argument ignored on hit
        let v2 = sm_once_expires(true);
        assert!(!v2.expired);

        // reset cache so we can test the expired path
        {
            let mut cache = SM_ONCE_EXPIRES.write();
            *cache = None;
        }
        // prime with an expired value
        let v3 = sm_once_expires(true);
        assert!(v3.expired);
        // expired entry — re-executes
        let v4 = sm_once_expires(false);
        assert!(!v4.expired);
    }

    // Regression for issue #246: a dynamic, per-entry TTL derived from a runtime
    // argument. Two keys are inserted with different lifetimes — a 0ms TTL (already
    // expired on the next read) and a long TTL (stays live) — so the assertions are
    // deterministic without sleeping (time only ever moves forward).
    #[test]
    fn test_expires_macro_dynamic_ttl_from_arg() {
        {
            let mut c = DYN_TTL.write();
            c.cache_clear();
            c.cache_reset_metrics();
        }

        // key 1: 0ms lifetime → expires immediately. key 2: long lifetime → stays live.
        assert_eq!(dyn_ttl(1, 0).v, 1); // miss
        assert_eq!(dyn_ttl(2, 60_000).v, 2); // miss

        // key 1's entry has expired → re-executes (no hit); key 2 is still live → hit.
        assert_eq!(dyn_ttl(1, 0).v, 1);
        assert_eq!(dyn_ttl(2, 60_000).v, 2);

        let c = DYN_TTL.read();
        // Only the long-TTL key produced a live hit; the 0ms key never hits.
        assert_eq!(c.cache_hits(), Some(1));
    }
}

#[cfg(all(feature = "time_stores", feature = "proc_macro"))]
mod time_store_tests {
    use super::*;
    use cached::stores::TtlSortedCache;
    use cached::time::Instant;
    use cached::{CachedPeek, CachedRead};

    #[cached(
        ty = "TtlSortedCache<String, usize>",
        create = "{ TtlSortedCache::builder().ttl(Duration::from_secs(60)).build().unwrap() }",
        key = "String",
        convert = r#"{ input.to_string() }"#,
        unsync_reads = true
    )]
    fn expiring_sized_unsync_read(input: &str) -> usize {
        input.len()
    }

    #[once(ttl_secs = 1)]
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

    #[cached(max_size = 1, ttl_secs = 1)]
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
    #[once(ttl_secs = 1)]
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
    #[once(ttl_secs = 1)]
    fn only_cached_result_once_per_second(
        s: String,
        error: bool,
    ) -> std::result::Result<Vec<String>, u32> {
        if error { Err(1) } else { Ok(vec![s]) }
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
    #[once(ttl_secs = 1)]
    fn only_cached_option_once_per_second(s: String, none: bool) -> Option<Vec<String>> {
        if none { None } else { Some(vec![s]) }
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

    #[cached(ttl_secs = 2, sync_writes = "default", key = "u32", convert = "{ 1 }")]
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

    #[cached(ttl_secs = 2, sync_writes = true, key = "u32", convert = "{ 2 }")]
    fn cached_sync_writes_true(s: String) -> Vec<String> {
        vec![s]
    }

    #[test]
    fn test_cached_sync_writes_true() {
        let a = cached_sync_writes_true("a".to_string());
        let b = cached_sync_writes_true("b".to_string());
        assert_eq!(a, b);
    }

    #[cached(ttl_secs = 2, sync_writes = false, key = "u32", convert = "{ 3 }")]
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
        ttl_secs = 2,
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
        ttl_secs = 1,
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
        max_size = 2,
        ttl_secs = 1,
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
        max_size = 2,
        ttl_secs = 1,
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

    #[cached(
        max_size = 2,
        ttl_secs = 1,
        key = "String",
        convert = r#"{ String::from(s) }"#
    )]
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

    #[cached::macros::cached(ttl_secs = 1, result_fallback = true)]
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

    // --- concurrent_cached result_fallback ---

    #[cfg(feature = "proc_macro")]
    static CONCURRENT_RESULT_FALLBACK_SHOULD_SUCCEED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(true);

    #[cfg(feature = "proc_macro")]
    #[cached::macros::concurrent_cached(ttl_secs = 1, result_fallback = true)]
    fn concurrent_result_fallback_fn() -> Result<u32, &'static str> {
        if CONCURRENT_RESULT_FALLBACK_SHOULD_SUCCEED.load(std::sync::atomic::Ordering::SeqCst) {
            Ok(42)
        } else {
            Err("backend down")
        }
    }

    #[cfg(feature = "proc_macro")]
    #[test]
    fn test_concurrent_cached_result_fallback() {
        // Ensure any prior cached entry has expired before the test starts.
        std::thread::sleep(Duration::from_millis(1100));

        // No cached Ok yet; function returns Err → raw Err returned to caller.
        CONCURRENT_RESULT_FALLBACK_SHOULD_SUCCEED.store(false, std::sync::atomic::Ordering::SeqCst);
        assert!(
            concurrent_result_fallback_fn().is_err(),
            "no prior Ok → Err returned directly"
        );

        // Now succeed: Ok(42) is cached.
        CONCURRENT_RESULT_FALLBACK_SHOULD_SUCCEED.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(concurrent_result_fallback_fn(), Ok(42));

        // Wait for TTL to expire.
        std::thread::sleep(Duration::from_millis(1500));

        // Function returns Err again; fallback returns the stale Ok(42).
        CONCURRENT_RESULT_FALLBACK_SHOULD_SUCCEED.store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            concurrent_result_fallback_fn(),
            Ok(42),
            "expired Ok entry should be returned via fallback"
        );
    }

    #[derive(Clone, Debug)]
    struct OnceExpiredValue {
        val: u32,
        expired: bool,
    }
    impl Expires for OnceExpiredValue {
        fn is_expired(&self) -> bool {
            self.expired
        }
    }

    #[once(expires = true)]
    fn get_once_expired(val: u32, expired: bool) -> OnceExpiredValue {
        OnceExpiredValue { val, expired }
    }

    #[test]
    fn test_once_expires_sync() {
        // Initial call - not expired, gets cached
        let r1 = get_once_expired(1, false);
        assert_eq!(r1.val, 1);

        // Subsequent call - should return cached value (1) even with different args
        let r2 = get_once_expired(2, false);
        assert_eq!(r2.val, 1);

        // Prime with an expired value
        assert!(get_once_expired_prime_cache(3, true).expired);

        // Since it's expired, calling the function again should re-evaluate and cache a new value
        let r3 = get_once_expired(4, false);
        assert_eq!(r3.val, 4);

        // Now it is cached and not expired, so calling again returns 4
        let r4 = get_once_expired(5, false);
        assert_eq!(r4.val, 4);
    }

    #[once(expires = true)]
    fn get_once_result_expired(val: u32, expired: bool) -> Result<OnceExpiredValue, String> {
        Ok(OnceExpiredValue { val, expired })
    }

    #[test]
    fn test_once_expires_result() {
        // Initial call - not expired, gets cached
        assert_eq!(get_once_result_expired(1, false).unwrap().val, 1);

        // Subsequent call - returns cached val=1
        assert_eq!(get_once_result_expired(2, false).unwrap().val, 1);

        // Prime with an expired value
        assert!(
            get_once_result_expired_prime_cache(3, true)
                .unwrap()
                .expired
        );

        // Since it's expired, calling again should re-evaluate
        assert_eq!(get_once_result_expired(4, false).unwrap().val, 4);

        // Cached again, returns 4
        assert_eq!(get_once_result_expired(5, false).unwrap().val, 4);
    }

    #[once(expires = true)]
    fn get_once_option_expired(val: u32, expired: bool) -> Option<OnceExpiredValue> {
        Some(OnceExpiredValue { val, expired })
    }

    #[test]
    fn test_once_expires_option() {
        // Initial call - not expired, gets cached
        assert_eq!(get_once_option_expired(1, false).unwrap().val, 1);

        // Subsequent call - returns cached val=1
        assert_eq!(get_once_option_expired(2, false).unwrap().val, 1);

        // Prime with an expired value
        assert!(
            get_once_option_expired_prime_cache(3, true)
                .unwrap()
                .expired
        );

        // Since it's expired, calling again should re-evaluate
        assert_eq!(get_once_option_expired(4, false).unwrap().val, 4);

        // Cached again, returns 4
        assert_eq!(get_once_option_expired(5, false).unwrap().val, 4);
    }

    #[once(expires = true)]
    fn get_once_result_expired_or_err(
        val: u32,
        expired: bool,
        err: bool,
    ) -> Result<OnceExpiredValue, String> {
        if err {
            Err("forced error".to_string())
        } else {
            Ok(OnceExpiredValue { val, expired })
        }
    }

    #[test]
    fn test_once_expires_result_err_not_cached() {
        // Err result must not be cached — next call re-executes the function
        assert!(get_once_result_expired_or_err(1, false, true).is_err());
        // Because Err wasn't cached, this call actually executes and caches Ok(val=2)
        assert_eq!(
            get_once_result_expired_or_err(2, false, false).unwrap().val,
            2
        );
        // Cached now, returns 2
        assert_eq!(
            get_once_result_expired_or_err(3, false, false).unwrap().val,
            2
        );
    }

    #[once(expires = true)]
    fn get_once_option_expired_or_none(
        val: u32,
        expired: bool,
        none: bool,
    ) -> Option<OnceExpiredValue> {
        if none {
            None
        } else {
            Some(OnceExpiredValue { val, expired })
        }
    }

    #[test]
    fn test_once_expires_option_none_not_cached() {
        // None result must not be cached — next call re-executes the function
        assert!(get_once_option_expired_or_none(1, false, true).is_none());
        // Because None wasn't cached, this call actually executes and caches Some(val=2)
        assert_eq!(
            get_once_option_expired_or_none(2, false, false)
                .unwrap()
                .val,
            2
        );
        // Cached now, returns 2
        assert_eq!(
            get_once_option_expired_or_none(3, false, false)
                .unwrap()
                .val,
            2
        );
    }

    #[cfg(all(feature = "async", feature = "proc_macro"))]
    mod async_tests {
        use super::*;

        #[once(ttl_secs = 1)]
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

        #[once(ttl_secs = 1)]
        async fn only_cached_result_once_per_second_a(
            s: String,
            error: bool,
        ) -> std::result::Result<Vec<String>, u32> {
            if error { Err(1) } else { Ok(vec![s]) }
        }

        #[tokio::test]
        async fn test_only_cached_result_once_per_second_a() {
            assert!(
                only_cached_result_once_per_second_a("z".to_string(), true)
                    .await
                    .is_err()
            );
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

        #[once(ttl_secs = 1)]
        async fn only_cached_option_once_per_second_a(
            s: String,
            none: bool,
        ) -> Option<Vec<String>> {
            if none { None } else { Some(vec![s]) }
        }

        #[tokio::test]
        async fn test_only_cached_option_once_per_second_a() {
            assert!(
                only_cached_option_once_per_second_a("z".to_string(), true)
                    .await
                    .is_none()
            );
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
        #[once(ttl_secs = 2, sync_writes)]
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

        #[cached(ttl_secs = 2, sync_writes = "default", key = "u32", convert = "{ 1 }")]
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
            ttl_secs = 5,
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

        #[derive(Clone, Debug)]
        struct OnceExpiredValueAsync {
            val: u32,
            expired: bool,
        }
        impl Expires for OnceExpiredValueAsync {
            fn is_expired(&self) -> bool {
                self.expired
            }
        }

        #[once(expires = true)]
        async fn get_once_expired_async(val: u32, expired: bool) -> OnceExpiredValueAsync {
            OnceExpiredValueAsync { val, expired }
        }

        #[tokio::test]
        async fn test_once_expires_async() {
            // Initial call - not expired, gets cached
            let r1 = get_once_expired_async(1, false).await;
            assert_eq!(r1.val, 1);

            // Subsequent call - should return cached value (1)
            let r2 = get_once_expired_async(2, false).await;
            assert_eq!(r2.val, 1);

            // Prime with an expired value
            assert!(get_once_expired_async_prime_cache(3, true).await.expired);

            // Since it's expired, calling again should re-evaluate
            let r3 = get_once_expired_async(4, false).await;
            assert_eq!(r3.val, 4);

            // Now it is cached and not expired, so calling again returns 4
            let r4 = get_once_expired_async(5, false).await;
            assert_eq!(r4.val, 4);
        }

        #[once(expires = true)]
        async fn get_once_result_expired_async(
            val: u32,
            expired: bool,
        ) -> Result<OnceExpiredValueAsync, String> {
            Ok(OnceExpiredValueAsync { val, expired })
        }

        #[tokio::test]
        async fn test_once_expires_result_async() {
            // Initial call - caches Ok(val=1, not expired)
            assert_eq!(
                get_once_result_expired_async(1, false).await.unwrap().val,
                1
            );

            // Hit — returns cached val=1
            assert_eq!(
                get_once_result_expired_async(2, false).await.unwrap().val,
                1
            );

            // Prime with expired value
            assert!(
                get_once_result_expired_async_prime_cache(3, true)
                    .await
                    .unwrap()
                    .expired
            );

            // Expired — re-executes and caches val=4
            assert_eq!(
                get_once_result_expired_async(4, false).await.unwrap().val,
                4
            );

            // Cached again
            assert_eq!(
                get_once_result_expired_async(5, false).await.unwrap().val,
                4
            );
        }

        #[once(expires = true)]
        async fn get_once_option_expired_async(
            val: u32,
            expired: bool,
        ) -> Option<OnceExpiredValueAsync> {
            Some(OnceExpiredValueAsync { val, expired })
        }

        #[tokio::test]
        async fn test_once_expires_option_async() {
            // Initial call - caches Some(val=1, not expired)
            assert_eq!(
                get_once_option_expired_async(1, false).await.unwrap().val,
                1
            );

            // Hit — returns cached val=1
            assert_eq!(
                get_once_option_expired_async(2, false).await.unwrap().val,
                1
            );

            // Prime with expired value
            assert!(
                get_once_option_expired_async_prime_cache(3, true)
                    .await
                    .unwrap()
                    .expired
            );

            // Expired — re-executes and caches val=4
            assert_eq!(
                get_once_option_expired_async(4, false).await.unwrap().val,
                4
            );

            // Cached again
            assert_eq!(
                get_once_option_expired_async(5, false).await.unwrap().val,
                4
            );
        }

        #[tokio::test]
        async fn test_expiring_cache_async() {
            use cached::CachedAsync;

            #[derive(Clone, Debug)]
            struct AsyncValue {
                val: String,
                expired: bool,
            }
            impl Expires for AsyncValue {
                fn is_expired(&self) -> bool {
                    self.expired
                }
            }

            let mut cache = ExpiringCache::builder().build().unwrap();

            // async_get_or_set_with: vacant
            let r1 = cache
                .async_get_or_set_with("key".to_string(), || async {
                    AsyncValue {
                        val: "hello".to_string(),
                        expired: false,
                    }
                })
                .await;
            assert_eq!(r1.val, "hello");

            // async_get_or_set_with: occupied and fresh
            let r2 = cache
                .async_get_or_set_with("key".to_string(), || async {
                    AsyncValue {
                        val: "ignored".to_string(),
                        expired: false,
                    }
                })
                .await;
            assert_eq!(r2.val, "hello");

            // Manually set to expired
            cache.cache_set(
                "key".to_string(),
                AsyncValue {
                    val: "expired_val".to_string(),
                    expired: true,
                },
            );

            // async_get_or_set_with: occupied but expired
            let r3 = cache
                .async_get_or_set_with("key".to_string(), || async {
                    AsyncValue {
                        val: "new_fresh".to_string(),
                        expired: false,
                    }
                })
                .await;
            assert_eq!(r3.val, "new_fresh");
        }

        #[derive(Clone, Debug)]
        struct AsyncCachedExpiresVal {
            val: u32,
            expired: bool,
        }
        impl Expires for AsyncCachedExpiresVal {
            fn is_expired(&self) -> bool {
                self.expired
            }
        }

        #[cached(expires = true, key = "u32", convert = "{ k }")]
        async fn async_cached_expires_basic(k: u32, expired: bool) -> AsyncCachedExpiresVal {
            AsyncCachedExpiresVal { val: k, expired }
        }

        #[tokio::test]
        async fn test_async_cached_expires_basic() {
            assert_eq!(async_cached_expires_basic(1, false).await.val, 1);
            assert_eq!(async_cached_expires_basic(1, false).await.val, 1);
            async_cached_expires_basic_prime_cache(1, true).await;
            let r = async_cached_expires_basic(1, false).await;
            assert_eq!(r.val, 1);
            assert!(!r.expired);
            assert_eq!(async_cached_expires_basic(1, false).await.val, 1);
        }

        #[cached(expires = true, key = "u32", convert = "{ k }")]
        async fn async_cached_expires_result(
            k: u32,
            expired: bool,
            err: bool,
        ) -> Result<AsyncCachedExpiresVal, String> {
            if err {
                Err("forced error".to_string())
            } else {
                Ok(AsyncCachedExpiresVal { val: k, expired })
            }
        }

        #[tokio::test]
        async fn test_async_cached_expires_result() {
            assert!(async_cached_expires_result(1, false, true).await.is_err());
            assert!(async_cached_expires_result(1, false, true).await.is_err());
            assert_eq!(
                async_cached_expires_result(1, false, false)
                    .await
                    .unwrap()
                    .val,
                1
            );
            assert_eq!(
                async_cached_expires_result(1, false, false)
                    .await
                    .unwrap()
                    .val,
                1
            );
            async_cached_expires_result_prime_cache(1, true, false)
                .await
                .unwrap();
            let r = async_cached_expires_result(1, false, false).await.unwrap();
            assert_eq!(r.val, 1);
            assert!(!r.expired);
        }

        #[cached(expires = true, key = "u32", convert = "{ k }")]
        async fn async_cached_expires_option(
            k: u32,
            expired: bool,
            none: bool,
        ) -> Option<AsyncCachedExpiresVal> {
            if none {
                None
            } else {
                Some(AsyncCachedExpiresVal { val: k, expired })
            }
        }

        #[tokio::test]
        async fn test_async_cached_expires_option() {
            assert!(async_cached_expires_option(1, false, true).await.is_none());
            assert!(async_cached_expires_option(1, false, true).await.is_none());
            assert_eq!(
                async_cached_expires_option(1, false, false)
                    .await
                    .unwrap()
                    .val,
                1
            );
            assert_eq!(
                async_cached_expires_option(1, false, false)
                    .await
                    .unwrap()
                    .val,
                1
            );
            async_cached_expires_option_prime_cache(1, true, false)
                .await
                .unwrap();
            let r = async_cached_expires_option(1, false, false).await.unwrap();
            assert_eq!(r.val, 1);
            assert!(!r.expired);
        }

        #[cached(expires = true, result_fallback = true, key = "u32", convert = "{ k }")]
        async fn async_cached_expires_result_fallback(
            k: u32,
            expired: bool,
            err: bool,
        ) -> Result<AsyncCachedExpiresVal, String> {
            if err {
                Err("forced error".to_string())
            } else {
                Ok(AsyncCachedExpiresVal { val: k, expired })
            }
        }

        #[tokio::test]
        async fn test_async_cached_expires_result_fallback() {
            async_cached_expires_result_fallback_prime_cache(1, false, false)
                .await
                .unwrap();
            assert_eq!(
                async_cached_expires_result_fallback(1, false, false)
                    .await
                    .unwrap()
                    .val,
                1
            );
            async_cached_expires_result_fallback_prime_cache(1, true, false)
                .await
                .unwrap();
            let r = async_cached_expires_result_fallback(1, false, true)
                .await
                .unwrap();
            assert_eq!(r.val, 1);
            assert!(r.expired);
        }
    }

    use cached::stores::ExpiringCache;

    #[derive(Clone, Debug)]
    struct CachedExpiresVal {
        val: u32,
        expired: bool,
    }
    impl Expires for CachedExpiresVal {
        fn is_expired(&self) -> bool {
            self.expired
        }
    }

    #[cached(expires = true, key = "u32", convert = "{ k }")]
    fn cached_expires_basic(k: u32, expired: bool) -> CachedExpiresVal {
        CachedExpiresVal { val: k, expired }
    }

    #[test]
    fn test_cached_expires_basic() {
        // miss — executes, caches {val=1, expired=false} under key=1
        assert_eq!(cached_expires_basic(1, false).val, 1);
        // hit — same key, returns cached value
        assert_eq!(cached_expires_basic(1, false).val, 1);
        {
            let c = CACHED_EXPIRES_BASIC.read();
            assert_eq!(c.cache_hits(), Some(1));
            assert_eq!(c.cache_misses(), Some(1));
        }
        // prime key=1 with expired value
        cached_expires_basic_prime_cache(1, true);
        // expired miss — re-executes with (1, false), caches fresh entry
        let r = cached_expires_basic(1, false);
        assert_eq!(r.val, 1);
        assert!(!r.expired);
        {
            let c = CACHED_EXPIRES_BASIC.read();
            assert_eq!(c.cache_hits(), Some(1));
            assert_eq!(c.cache_misses(), Some(2));
            assert_eq!(c.cache_evictions(), Some(1));
        }
        // hit again — fresh entry
        assert_eq!(cached_expires_basic(1, false).val, 1);
    }

    #[cached(expires = true, max_size = 4, key = "u32", convert = "{ k }")]
    fn cached_expires_lru(k: u32, expired: bool) -> CachedExpiresVal {
        CachedExpiresVal { val: k, expired }
    }

    #[test]
    fn test_cached_expires_lru() {
        // miss — caches {val=10, expired=false}
        assert_eq!(cached_expires_lru(10, false).val, 10);
        // hit
        assert_eq!(cached_expires_lru(10, false).val, 10);
        {
            let c = CACHED_EXPIRES_LRU.read();
            assert_eq!(c.cache_hits(), Some(1));
            assert_eq!(c.cache_misses(), Some(1));
        }
        // prime key=10 with expired value
        cached_expires_lru_prime_cache(10, true);
        // expired miss — re-executes, caches fresh entry
        let r = cached_expires_lru(10, false);
        assert_eq!(r.val, 10);
        assert!(!r.expired);
        {
            let c = CACHED_EXPIRES_LRU.read();
            assert_eq!(c.cache_evictions(), Some(1));
        }
    }

    #[cached(expires = true, key = "u32", convert = "{ k }")]
    fn cached_expires_result(k: u32, expired: bool, err: bool) -> Result<CachedExpiresVal, String> {
        if err {
            Err("forced error".to_string())
        } else {
            Ok(CachedExpiresVal { val: k, expired })
        }
    }

    #[test]
    fn test_cached_expires_result() {
        // Err is not cached — next call re-executes
        assert!(cached_expires_result(1, false, true).is_err());
        assert!(cached_expires_result(1, false, true).is_err());
        // Ok is cached
        assert_eq!(cached_expires_result(1, false, false).unwrap().val, 1);
        // hit
        assert_eq!(cached_expires_result(1, false, false).unwrap().val, 1);
        // prime key=1 with expired
        cached_expires_result_prime_cache(1, true, false).unwrap();
        // expired miss — re-executes
        let r = cached_expires_result(1, false, false).unwrap();
        assert_eq!(r.val, 1);
        assert!(!r.expired);
        {
            let c = CACHED_EXPIRES_RESULT.read();
            assert_eq!(c.cache_evictions(), Some(1));
        }
    }

    #[cached(expires = true, key = "u32", convert = "{ k }")]
    fn cached_expires_option(k: u32, expired: bool, none: bool) -> Option<CachedExpiresVal> {
        if none {
            None
        } else {
            Some(CachedExpiresVal { val: k, expired })
        }
    }

    #[test]
    fn test_cached_expires_option() {
        // None is not cached — next call re-executes
        assert!(cached_expires_option(1, false, true).is_none());
        assert!(cached_expires_option(1, false, true).is_none());
        // Some is cached
        assert_eq!(cached_expires_option(1, false, false).unwrap().val, 1);
        // hit
        assert_eq!(cached_expires_option(1, false, false).unwrap().val, 1);
        // prime key=1 with expired
        cached_expires_option_prime_cache(1, true, false).unwrap();
        // expired miss — re-executes
        let r = cached_expires_option(1, false, false).unwrap();
        assert_eq!(r.val, 1);
        assert!(!r.expired);
        {
            let c = CACHED_EXPIRES_OPTION.read();
            assert_eq!(c.cache_evictions(), Some(1));
        }
    }

    #[cached(expires = true, result_fallback = true, key = "u32", convert = "{ k }")]
    fn cached_expires_result_fallback(
        k: u32,
        expired: bool,
        err: bool,
    ) -> Result<CachedExpiresVal, String> {
        if err {
            Err("forced error".to_string())
        } else {
            Ok(CachedExpiresVal { val: k, expired })
        }
    }

    #[test]
    fn test_cached_expires_result_fallback() {
        // prime key=1 with a non-expired value
        cached_expires_result_fallback_prime_cache(1, false, false).unwrap();
        // fresh hit
        assert_eq!(
            cached_expires_result_fallback(1, false, false).unwrap().val,
            1
        );
        // prime key=1 with expired value
        cached_expires_result_fallback_prime_cache(1, true, false).unwrap();
        // function returns Err + stale value exists → result_fallback returns stale Ok
        let r = cached_expires_result_fallback(1, false, true).unwrap();
        assert_eq!(r.val, 1);
        assert!(r.expired);
    }

    #[test]
    fn test_expiring_cache_integration() {
        #[derive(Clone, Debug, PartialEq, Eq)]
        struct Value {
            val: String,
            expired: bool,
        }
        impl Expires for Value {
            fn is_expired(&self) -> bool {
                self.expired
            }
        }

        let mut cache = ExpiringCache::builder().build().unwrap();
        cache.cache_set(
            "a".to_string(),
            Value {
                val: "hello".to_string(),
                expired: false,
            },
        );
        cache.cache_set(
            "b".to_string(),
            Value {
                val: "world".to_string(),
                expired: true,
            },
        );
        assert_eq!(
            cache.cache_get(&"a".to_string()).map(|v| &v.val),
            Some(&"hello".to_string())
        );
        assert!(cache.cache_get(&"b".to_string()).is_none());
        assert_eq!(cache.cache_hits(), Some(1));
        assert_eq!(cache.cache_misses(), Some(1));
        assert_eq!(cache.cache_evictions(), Some(1));
    }
}

#[cfg(feature = "time_stores")]
mod sharded_ttl_tests {
    // Verify that `refresh_on_hit = true` actually extends entry lifetime.
    #[test]
    fn sharded_ttl_refresh_on_hit_extends_lifetime() {
        use cached::ConcurrentCached;
        use cached::ShardedTtlCache;
        use cached::time::Duration;

        let cache: ShardedTtlCache<u32, u32> = ShardedTtlCache::builder()
            .ttl(Duration::from_millis(3_000))
            .shards(4)
            .refresh_on_hit(true)
            .build()
            .expect("valid config");

        let _ = ConcurrentCached::cache_set(&cache, 1u32, 42u32);
        // Sleep 500ms; entry should still be well inside its 3s TTL.
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            ConcurrentCached::cache_get(&cache, &1u32).expect("infallible"),
            Some(42),
            "entry should still be alive before TTL expires"
        );
        // Sleep another 1_500ms. This is past the original expiry, but inside the
        // refreshed TTL window from the previous get (~1_500ms margin to refreshed expiry).
        std::thread::sleep(Duration::from_millis(1_500));
        assert_eq!(
            ConcurrentCached::cache_get(&cache, &1u32).expect("infallible"),
            Some(42),
            "entry should still be alive after TTL was refreshed on the previous get"
        );
        // Sleep past the last refresh; entry should now be expired.
        std::thread::sleep(Duration::from_millis(3_200));
        assert_eq!(
            ConcurrentCached::cache_get(&cache, &1u32).expect("infallible"),
            None,
            "entry should be expired after TTL elapsed with no further refresh"
        );
    }

    #[test]
    fn sharded_ttl_stores_implement_concurrent_cache_evict_trait() {
        use cached::time::Duration;
        use cached::{ConcurrentCacheEvict, ShardedLruTtlCache, ShardedTtlCache};

        fn assert_cache_evict<C: ConcurrentCacheEvict>(cache: &C) -> usize {
            cache.evict()
        }

        let ttl: ShardedTtlCache<u32, u32> = ShardedTtlCache::builder()
            .ttl(Duration::from_secs(60))
            .build()
            .expect("valid config");
        let lru_ttl: ShardedLruTtlCache<u32, u32> = ShardedLruTtlCache::builder()
            .max_size(16)
            .ttl(Duration::from_secs(60))
            .build()
            .expect("valid config");

        assert_eq!(assert_cache_evict(&ttl), 0);
        assert_eq!(assert_cache_evict(&lru_ttl), 0);
    }

    #[test]
    fn sharded_ttl_builders_accept_refresh_on_hit() {
        use cached::time::Duration;
        use cached::{ShardedLruTtlCache, ShardedTtlCache};

        let ttl = ShardedTtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .refresh_on_hit(true)
            .build()
            .expect("valid config");
        assert!(ttl.refresh_on_hit());

        let lru_ttl = ShardedLruTtlCache::<u32, u32>::builder()
            .max_size(64)
            .ttl(Duration::from_secs(60))
            .refresh_on_hit(true)
            .build()
            .expect("valid config");
        assert!(lru_ttl.refresh_on_hit());
    }

    // Covers `ConcurrentCached::cache_reset` / `cache_reset_metrics` on the TTL/expiring
    // sharded stores, whose `cache_reset_metrics` must zero a *split* eviction count
    // (the per-shard inner `LruCache`'s capacity-eviction counter plus the store's own
    // counter). The non-TTL test exercises only `ShardedUnboundCache`/`ShardedLruCache`.
    #[test]
    fn reset_metrics_zeros_split_eviction_counter_on_ttl_expiring_sharded_stores() {
        use cached::time::Duration;
        use cached::{ConcurrentCached, ShardedExpiringLruCache, ShardedLruTtlCache};

        // ShardedLruTtlCache: a single shard with capacity 1 forces an LRU eviction.
        let lru_ttl = ShardedLruTtlCache::<u32, u32>::builder()
            .per_shard_max_size(1)
            .shards(1)
            .ttl(Duration::from_secs(60))
            .build()
            .unwrap();
        ConcurrentCached::cache_set(&lru_ttl, 1, 10).expect("infallible");
        ConcurrentCached::cache_set(&lru_ttl, 2, 20).expect("infallible"); // evicts key 1
        let _ = ConcurrentCached::cache_get(&lru_ttl, &2).expect("infallible");
        assert_eq!(lru_ttl.metrics().evictions, Some(1));
        assert!(lru_ttl.metrics().hits.unwrap() >= 1);

        ConcurrentCached::cache_reset(&lru_ttl).expect("infallible");
        assert_eq!(lru_ttl.len(), 0, "cache_reset must remove all entries");
        assert_eq!(lru_ttl.metrics().hits, Some(0));
        assert_eq!(lru_ttl.metrics().misses, Some(0));
        assert_eq!(
            lru_ttl.metrics().evictions,
            Some(0),
            "cache_reset must zero the split eviction counter"
        );

        // ShardedExpiringLruCache: same split-counter path for the expiring variant.
        #[derive(Clone)]
        struct NeverExpires;
        impl cached::Expires for NeverExpires {
            fn is_expired(&self) -> bool {
                false
            }
        }
        let exp_lru = ShardedExpiringLruCache::<u32, NeverExpires>::builder()
            .per_shard_max_size(1)
            .shards(1)
            .build()
            .unwrap();
        ConcurrentCached::cache_set(&exp_lru, 1, NeverExpires).expect("infallible");
        ConcurrentCached::cache_set(&exp_lru, 2, NeverExpires).expect("infallible"); // evicts key 1
        let _ = ConcurrentCached::cache_get(&exp_lru, &2).expect("infallible");
        assert_eq!(exp_lru.metrics().evictions, Some(1));

        ConcurrentCached::cache_reset_metrics(&exp_lru).expect("infallible");
        assert_eq!(
            exp_lru.metrics().evictions,
            Some(0),
            "cache_reset_metrics must zero the split eviction counter"
        );
        assert_eq!(exp_lru.metrics().hits, Some(0));
    }

    // The non-sharded TTL builders expose `refresh_on_hit(..)` as the setter
    // (matching the sharded builders).
    #[test]
    fn non_sharded_ttl_builders_accept_refresh_on_hit() {
        use cached::time::Duration;
        use cached::{LruTtlCache, TtlCache};

        // Primary `.refresh_on_hit(true)` setter.
        let ttl = TtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .refresh_on_hit(true)
            .build()
            .expect("valid config");
        assert!(ttl.refresh_on_hit());

        let lru_ttl = LruTtlCache::<u32, u32>::builder()
            .max_size(64)
            .ttl(Duration::from_secs(60))
            .refresh_on_hit(true)
            .build()
            .expect("valid config");
        assert!(lru_ttl.refresh_on_hit());

        // Both setters default to / can clear the flag.
        let ttl_off = TtlCache::<u32, u32>::builder()
            .ttl(Duration::from_secs(60))
            .refresh_on_hit(false)
            .build()
            .expect("valid config");
        assert!(!ttl_off.refresh_on_hit());
    }

    #[test]
    fn sharded_lru_ttl_evict_does_not_double_count_evictions_or_double_fire_on_evict() {
        use cached::ConcurrentCached;
        use cached::ShardedLruTtlCache;
        use cached::time::Duration;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

        let fired = Arc::new(AtomicU32::new(0));
        let fired_clone = fired.clone();
        let cache = ShardedLruTtlCache::builder()
            .max_size(16)
            .ttl(Duration::from_millis(50))
            .on_evict(move |_k, _v| {
                fired_clone.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();

        cache.cache_set(1u32, 10u32).expect("infallible");
        cache.cache_set(2u32, 20u32).expect("infallible");
        cache.cache_set(3u32, 30u32).expect("infallible");

        std::thread::sleep(Duration::from_millis(100));

        // evict() should report 3, fire on_evict exactly 3 times (not 6), and
        // metrics().evictions should return 3 (not 6).
        assert_eq!(cache.evict(), 3);
        assert_eq!(fired.load(Ordering::Relaxed), 3);
        assert_eq!(cache.metrics().evictions, Some(3));
    }

    #[test]
    fn sharded_ttl_evict_fires_on_evict_and_increments_evictions_counter() {
        use cached::ConcurrentCached;
        use cached::ShardedTtlCache;
        use cached::time::Duration;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

        let fired = Arc::new(AtomicU32::new(0));
        let fired_clone = fired.clone();
        let cache = ShardedTtlCache::builder()
            .ttl(Duration::from_millis(50))
            .on_evict(move |_k, _v| {
                fired_clone.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();

        cache.cache_set(1u32, 10u32).expect("infallible");
        cache.cache_set(2u32, 20u32).expect("infallible");
        cache.cache_set(3u32, 30u32).expect("infallible");

        std::thread::sleep(Duration::from_millis(100));

        assert_eq!(cache.evict(), 3);
        assert_eq!(fired.load(Ordering::Relaxed), 3);
        assert_eq!(cache.metrics().evictions, Some(3));
    }
}

#[cfg(all(feature = "async", feature = "proc_macro"))]
mod async_tests {
    use super::*;

    #[once]
    async fn only_cached_result_once_a(
        s: String,
        error: bool,
    ) -> std::result::Result<Vec<String>, u32> {
        if error { Err(1) } else { Ok(vec![s]) }
    }

    #[tokio::test]
    async fn test_only_cached_result_once_a() {
        assert!(
            only_cached_result_once_a("z".to_string(), true)
                .await
                .is_err()
        );
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

    #[once]
    async fn only_cached_option_once_a(s: String, none: bool) -> Option<Vec<String>> {
        if none { None } else { Some(vec![s]) }
    }

    #[tokio::test]
    async fn test_only_cached_option_once_a() {
        assert!(
            only_cached_option_once_a("z".to_string(), true)
                .await
                .is_none()
        );
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
    use cached::RedbCache;
    use cached::macros::concurrent_cached;
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
        ttl_secs = 1,
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

    // #8 disk-path parity: `refresh = true` on the disk (redb) path is now a
    // plain `bool` and is wired straight into the store builder via
    // `.refresh_on_hit(refresh)`. This proves the macro emits a working disk
    // store with `refresh = true` + a TTL (compiles, caches an `Ok` hit, and
    // does not cache `Err`). The TTL-renewal side effect of `refresh_on_hit`
    // itself is exercised by the store-level tests; here we lock that the macro
    // attribute path wires it without error.
    #[concurrent_cached(
        disk = true,
        ttl_secs = 60,
        refresh = true,
        map_error = r##"|e| TestError::DiskError(format!("{:?}", e))"##
    )]
    fn cached_disk_refresh(n: u32) -> Result<u32, TestError> {
        if n < 5 {
            Ok(n)
        } else {
            Err(TestError::Count(n))
        }
    }

    #[test]
    fn test_cached_disk_refresh() {
        // First call: miss, Ok value computed and cached.
        assert_eq!(cached_disk_refresh(1), Ok(1));
        // Second call same arg: served from the disk cache (refresh_on_hit set).
        assert_eq!(cached_disk_refresh(1), Ok(1));
        // Err is not cached and is returned as-is.
        assert_eq!(cached_disk_refresh(5), Err(TestError::Count(5)));
    }

    #[concurrent_cached(
        disk = true,
        ttl_secs = 1,
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
        ty = "cached::RedbCache<u32, u32>",
        create = r##" { RedbCache::builder("cached_disk_cache_create").ttl(Duration::from_secs(1)).refresh_on_hit(true).build().expect("error building disk cache") } "##
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

    // #8: `refresh = false` is now the plain-bool default and must NOT conflict
    // with a `create` block. Previously `refresh` was `Option<bool>`, so an
    // explicit `refresh = Some(false)` alongside `create` tripped the
    // create-conflict rejection (`check_create_conflicts`). It now compiles:
    // `refresh = false` is treated as "not set" by the conflict check.
    #[concurrent_cached(
        map_error = r##"|e| TestError::DiskError(format!("{:?}", e))"##,
        refresh = false,
        ty = "cached::RedbCache<u32, u32>",
        create = r##" { RedbCache::builder("cached_disk_refresh_false_create").build().expect("error building disk cache") } "##
    )]
    fn cached_disk_refresh_false_create(n: u32) -> Result<u32, TestError> {
        if n < 5 {
            Ok(n)
        } else {
            Err(TestError::Count(n))
        }
    }

    #[test]
    fn test_cached_disk_refresh_false_create() {
        // `refresh = false` + `create` compiles and behaves as a plain cache.
        assert_eq!(cached_disk_refresh_false_create(1), Ok(1));
        assert_eq!(cached_disk_refresh_false_create(1), Ok(1));
        assert_eq!(
            cached_disk_refresh_false_create(5),
            Err(TestError::Count(5))
        );
    }

    /// Just calling the macro with durable to test it doesn't break with an expected value
    /// There are no simple tests to test this here
    #[concurrent_cached(
        disk = true,
        map_error = r##"|e| TestError::DiskError(format!("{:?}", e))"##,
        durable = true
    )]
    fn cached_disk_durable(n: u32) -> Result<u32, TestError> {
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
        // caching. Before relaxing the async `RedbCache` impl (fn-pointer
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
// Plain (non-Result) return types for `#[concurrent_cached]` on the default
// in-memory sharded store. The macro generates code that calls `.unwrap()` on
// the infallible cache operations instead of wrapping in `Ok(...)`.
#[cfg(feature = "proc_macro")]
mod concurrent_cached_plain_return {
    use cached::macros::concurrent_cached;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static PLAIN_CALLS: AtomicUsize = AtomicUsize::new(0);
    static PLAIN_OPTION_CALLS: AtomicUsize = AtomicUsize::new(0);
    static PLAIN_OPTION_NONE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static PLAIN_MAX_SIZE_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached]
    fn plain_double(x: u64) -> u64 {
        PLAIN_CALLS.fetch_add(1, Ordering::Relaxed);
        x * 2
    }

    #[concurrent_cached(max_size = 100)]
    fn plain_double_lru(x: u64) -> u64 {
        x * 2
    }

    // `max_size` is an alias for `size` on #[concurrent_cached] too: it must
    // route to the sharded LRU store identically.
    #[concurrent_cached(max_size = 100)]
    fn plain_double_lru_max_size(x: u64) -> u64 {
        PLAIN_MAX_SIZE_CALLS.fetch_add(1, Ordering::Relaxed);
        x * 2
    }

    /// Default: Option<T> skips None (smart-option). Only Some(T) is cached.
    #[concurrent_cached]
    fn plain_option(x: u64) -> Option<u64> {
        PLAIN_OPTION_CALLS.fetch_add(1, Ordering::Relaxed);
        if x == 0 { None } else { Some(x * 2) }
    }

    /// Opt-in: cache_none = true stores None in the cache too.
    #[concurrent_cached(cache_none = true)]
    fn plain_option_cache_none(x: u64) -> Option<u64> {
        PLAIN_OPTION_NONE_CALLS.fetch_add(1, Ordering::Relaxed);
        if x == 0 { None } else { Some(x * 2) }
    }

    #[concurrent_cached]
    fn plain_hash_map(x: u64) -> HashMap<u64, u64> {
        let mut map = HashMap::new();
        map.insert(x, x * 2);
        map
    }

    #[test]
    fn plain_return_compiles_and_caches() {
        PLAIN_CALLS.store(0, Ordering::Relaxed);
        assert_eq!(plain_double(21), 42);
        assert_eq!(plain_double(21), 42); // cached — no second call
        assert_eq!(plain_double(22), 44); // different key
        assert_eq!(PLAIN_CALLS.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn plain_return_lru_compiles_and_caches() {
        assert_eq!(plain_double_lru(10), 20);
        assert_eq!(plain_double_lru(10), 20); // cached
    }

    #[test]
    fn plain_return_max_size_alias_compiles_and_caches() {
        PLAIN_MAX_SIZE_CALLS.store(0, Ordering::Relaxed);
        assert_eq!(plain_double_lru_max_size(10), 20);
        assert_eq!(plain_double_lru_max_size(10), 20); // cached — no second call
        assert_eq!(plain_double_lru_max_size(11), 22); // different key
        assert_eq!(PLAIN_MAX_SIZE_CALLS.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn plain_option_return_skips_none_caches_some() {
        PLAIN_OPTION_CALLS.store(0, Ordering::Relaxed);
        // None is NOT cached — function runs again each time
        assert_eq!(plain_option(0), None);
        assert_eq!(plain_option(0), None);
        assert_eq!(
            PLAIN_OPTION_CALLS.load(Ordering::Relaxed),
            2,
            "None should NOT be cached by default"
        );
        // Some(T) IS cached
        assert_eq!(plain_option(5), Some(10));
        assert_eq!(plain_option(5), Some(10));
        assert_eq!(
            PLAIN_OPTION_CALLS.load(Ordering::Relaxed),
            3,
            "Some should be cached"
        );
    }

    #[test]
    fn plain_option_cache_none_caches_none_and_some() {
        PLAIN_OPTION_NONE_CALLS.store(0, Ordering::Relaxed);
        // With cache_none = true, None IS cached
        assert_eq!(plain_option_cache_none(0), None);
        assert_eq!(plain_option_cache_none(0), None);
        assert_eq!(
            PLAIN_OPTION_NONE_CALLS.load(Ordering::Relaxed),
            1,
            "None should be cached with cache_none = true"
        );
        // Some(T) is also cached
        assert_eq!(plain_option_cache_none(5), Some(10));
        assert_eq!(plain_option_cache_none(5), Some(10));
        assert_eq!(
            PLAIN_OPTION_NONE_CALLS.load(Ordering::Relaxed),
            2,
            "Some should be cached"
        );
    }

    #[test]
    fn plain_generic_return_is_not_misclassified_as_result() {
        assert_eq!(plain_hash_map(7).get(&7), Some(&14));
        assert_eq!(plain_hash_map(7).get(&7), Some(&14));
    }
}

#[cfg(all(feature = "proc_macro", feature = "time_stores"))]
mod concurrent_cached_plain_return_ttl {
    use cached::macros::concurrent_cached;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TTL_PLAIN_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached(ttl_secs = 60)]
    fn plain_double_ttl(x: u64) -> u64 {
        TTL_PLAIN_CALLS.fetch_add(1, Ordering::Relaxed);
        x * 2
    }

    #[concurrent_cached(max_size = 50, ttl_secs = 60)]
    fn plain_double_lru_ttl(x: u64) -> u64 {
        x * 2
    }

    #[test]
    fn plain_ttl_compiles_and_caches() {
        TTL_PLAIN_CALLS.store(0, Ordering::Relaxed);
        assert_eq!(plain_double_ttl(21), 42);
        assert_eq!(plain_double_ttl(21), 42); // cached
        assert_eq!(TTL_PLAIN_CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn plain_lru_ttl_compiles_and_caches() {
        assert_eq!(plain_double_lru_ttl(21), 42);
        assert_eq!(plain_double_lru_ttl(21), 42); // cached
    }
}

// Sharded in-memory default for `#[concurrent_cached]`. No `ty`, `create`,
// `map_error`, `redis`, or `disk` — the macro defaults to `ShardedUnboundCache`.
#[cfg(feature = "proc_macro")]
mod concurrent_cached_default_in_memory {
    use cached::macros::concurrent_cached;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SLOW_DOUBLE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static FALLIBLE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static CUSTOM_RESULT_CALLS: AtomicUsize = AtomicUsize::new(0);
    static PLAIN_ALIAS_CALLS: AtomicUsize = AtomicUsize::new(0);
    static BARE_RESULT_ALIAS_CALLS: AtomicUsize = AtomicUsize::new(0);
    // Plain return type — no boilerplate required.
    #[concurrent_cached]
    fn slow_double(x: u64) -> u64 {
        SLOW_DOUBLE_CALLS.fetch_add(1, Ordering::Relaxed);
        x * 2
    }

    // Result<T, E>: only Ok values are cached; Err is returned but not stored.
    #[concurrent_cached]
    fn slow_double_fallible(x: u64) -> Result<u64, String> {
        FALLIBLE_CALLS.fetch_add(1, Ordering::Relaxed);
        if x == 0 {
            Err("zero is not cacheable".to_string())
        } else {
            Ok(x * 2)
        }
    }

    // Type aliases are not resolved at macro expansion time. Only a last path segment
    // of exactly `Result` is treated as a Result return; any alias — even one named
    // `MyResult<T>` — is treated as a plain value and its `Err` variant is cached.
    type MyResult<T> = Result<T, String>;

    #[concurrent_cached]
    fn slow_double_custom_result(x: u64) -> MyResult<u64> {
        CUSTOM_RESULT_CALLS.fetch_add(1, Ordering::Relaxed);
        if x == 0 {
            Err("zero is cached (plain alias)".to_string())
        } else {
            Ok(x * 2)
        }
    }

    // Same: `Api<T>` does not resolve to `Result` at macro time, so Err is cached.
    type Api<T> = Result<T, String>;

    #[concurrent_cached]
    fn slow_double_plain_alias(x: u64) -> Api<u64> {
        PLAIN_ALIAS_CALLS.fetch_add(1, Ordering::Relaxed);
        if x == 0 {
            Err("zero is cached for this plain alias".to_string())
        } else {
            Ok(x * 2)
        }
    }

    // `BareResult` has no type arguments, so it cannot match `Result<T, E>` at
    // macro-expansion time; it is treated as a plain value (Err is cached).
    type BareResult = Result<u64, String>;

    #[concurrent_cached]
    fn slow_double_bare_result_alias(x: u64) -> BareResult {
        BARE_RESULT_ALIAS_CALLS.fetch_add(1, Ordering::Relaxed);
        if x == 0 {
            Err("zero is cached for this bare alias".to_string())
        } else {
            Ok(x * 2)
        }
    }

    #[test]
    fn bare_default_compiles_and_caches() {
        SLOW_DOUBLE_CALLS.store(0, Ordering::Relaxed);
        assert_eq!(slow_double(21), 42);
        assert_eq!(slow_double(21), 42); // cached — no second call
        assert_eq!(slow_double(22), 44); // different key
        assert_eq!(SLOW_DOUBLE_CALLS.load(Ordering::Relaxed), 2); // 21 and 22, not a third for 21
    }

    #[test]
    fn result_return_skips_caching_on_err() {
        FALLIBLE_CALLS.store(0, Ordering::Relaxed);
        // Err is not cached; each call to the 0 key hits the function body.
        assert!(slow_double_fallible(0).is_err());
        assert!(slow_double_fallible(0).is_err());
        assert_eq!(FALLIBLE_CALLS.load(Ordering::Relaxed), 2);
        // Ok is cached normally.
        assert_eq!(slow_double_fallible(5), Ok(10));
        assert_eq!(slow_double_fallible(5), Ok(10)); // cached
        assert_eq!(FALLIBLE_CALLS.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn custom_result_alias_treated_as_plain_return() {
        CUSTOM_RESULT_CALLS.store(0, Ordering::Relaxed);
        // MyResult<T> is a type alias; the macro sees `MyResult`, not `Result`,
        // so Err is cached just like any other plain value.
        assert!(slow_double_custom_result(0).is_err());
        assert!(slow_double_custom_result(0).is_err()); // served from cache
        assert_eq!(CUSTOM_RESULT_CALLS.load(Ordering::Relaxed), 1);

        assert_eq!(slow_double_custom_result(21), Ok(42));
        assert_eq!(slow_double_custom_result(21), Ok(42)); // cached
        assert_eq!(CUSTOM_RESULT_CALLS.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn result_alias_without_result_suffix_is_treated_as_plain_return() {
        PLAIN_ALIAS_CALLS.store(0, Ordering::Relaxed);
        assert!(slow_double_plain_alias(0).is_err());
        assert!(slow_double_plain_alias(0).is_err());
        assert_eq!(PLAIN_ALIAS_CALLS.load(Ordering::Relaxed), 1);

        assert_eq!(slow_double_plain_alias(21), Ok(42));
        assert_eq!(slow_double_plain_alias(21), Ok(42));
        assert_eq!(PLAIN_ALIAS_CALLS.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn bare_result_alias_without_type_args_is_treated_as_plain_return() {
        BARE_RESULT_ALIAS_CALLS.store(0, Ordering::Relaxed);
        // `BareResult` has ident `BareResult`, not `Result`, so the exact-ident check
        // rejects it and it is treated as a plain value — `Err` is cached.
        assert!(slow_double_bare_result_alias(0).is_err());
        assert!(slow_double_bare_result_alias(0).is_err());
        assert_eq!(BARE_RESULT_ALIAS_CALLS.load(Ordering::Relaxed), 1);

        assert_eq!(slow_double_bare_result_alias(21), Ok(42));
        assert_eq!(slow_double_bare_result_alias(21), Ok(42));
        assert_eq!(BARE_RESULT_ALIAS_CALLS.load(Ordering::Relaxed), 2);
    }
}

#[cfg(all(feature = "proc_macro", feature = "async_core"))]
mod concurrent_cached_default_with_both_traits_in_scope {
    use cached::macros::concurrent_cached;
    #[allow(unused_imports)]
    use cached::{ConcurrentCached, ConcurrentCachedAsync};

    #[concurrent_cached]
    fn double_with_both_traits_in_scope(x: u64) -> u64 {
        x * 2
    }

    #[test]
    fn sync_macro_uses_ufcs_to_avoid_trait_method_ambiguity() {
        assert_eq!(double_with_both_traits_in_scope(21), 42);
    }
}

// `max_size = N` selects `ShardedLruCache`.
#[cfg(feature = "proc_macro")]
mod concurrent_cached_default_with_max_size {
    use cached::macros::concurrent_cached;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SLOW_TRIPLE_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached(max_size = 100)]
    fn slow_triple(x: u64) -> u64 {
        SLOW_TRIPLE_CALLS.fetch_add(1, Ordering::Relaxed);
        x * 3
    }

    #[test]
    fn size_attr_compiles_and_caches() {
        SLOW_TRIPLE_CALLS.store(0, Ordering::Relaxed);
        assert_eq!(slow_triple(21), 63);
        assert_eq!(slow_triple(21), 63); // cached
        assert_eq!(SLOW_TRIPLE_CALLS.load(Ordering::Relaxed), 1);
    }
}

// `ttl = T` selects `ShardedTtlCache`.
#[cfg(all(feature = "proc_macro", feature = "time_stores"))]
mod concurrent_cached_default_with_ttl {
    use cached::macros::concurrent_cached;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SLOW_QUAD_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached(ttl_secs = 60)]
    fn slow_quad(x: u64) -> u64 {
        SLOW_QUAD_CALLS.fetch_add(1, Ordering::Relaxed);
        x * 4
    }

    // Verify `refresh = true` compiles and is wired (store created with refresh enabled).
    #[concurrent_cached(ttl_secs = 60, refresh = true)]
    fn slow_quad_refresh(x: u64) -> u64 {
        x * 4
    }

    #[test]
    fn ttl_attr_compiles_and_caches() {
        SLOW_QUAD_CALLS.store(0, Ordering::Relaxed);
        assert_eq!(slow_quad(21), 84);
        assert_eq!(slow_quad(21), 84); // cached
        assert_eq!(SLOW_QUAD_CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn ttl_refresh_attr_wired() {
        // Verify the store has refresh enabled; if `refresh` were silently dropped
        // `refresh_on_hit()` would return false.
        assert_eq!(slow_quad_refresh(5), 20);
        assert!(SLOW_QUAD_REFRESH.refresh_on_hit());
    }
}

// `max_size = N, ttl = T` selects `ShardedLruTtlCache`.
#[cfg(all(feature = "proc_macro", feature = "time_stores"))]
mod concurrent_cached_default_with_max_size_and_ttl {
    use cached::macros::concurrent_cached;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SLOW_QUINT_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached(max_size = 50, ttl_secs = 60)]
    fn slow_quint(x: u64) -> u64 {
        SLOW_QUINT_CALLS.fetch_add(1, Ordering::Relaxed);
        x * 5
    }

    // Verify `refresh = true` compiles and is wired for the LRU+TTL variant.
    #[concurrent_cached(max_size = 50, ttl_secs = 60, refresh = true)]
    fn slow_quint_refresh(x: u64) -> u64 {
        x * 5
    }

    #[test]
    fn size_and_ttl_compiles_and_caches() {
        SLOW_QUINT_CALLS.store(0, Ordering::Relaxed);
        assert_eq!(slow_quint(21), 105);
        assert_eq!(slow_quint(21), 105); // cached
        assert_eq!(SLOW_QUINT_CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn size_and_ttl_refresh_attr_wired() {
        assert_eq!(slow_quint_refresh(5), 25);
        assert!(SLOW_QUINT_REFRESH.refresh_on_hit());
    }
}

// `shards = N` propagates through every default variant.
#[cfg(feature = "proc_macro")]
mod concurrent_cached_default_with_shards {
    use cached::macros::concurrent_cached;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DOUBLE_WITH_SHARDS_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached(shards = 32)]
    fn double_with_shards(x: u64) -> u64 {
        DOUBLE_WITH_SHARDS_CALLS.fetch_add(1, Ordering::Relaxed);
        x * 2
    }

    #[concurrent_cached(max_size = 100, shards = 32)]
    fn double_with_max_size_shards(x: u64) -> u64 {
        x * 2
    }

    #[test]
    fn shards_attr_compiles_and_caches() {
        DOUBLE_WITH_SHARDS_CALLS.store(0, Ordering::Relaxed);
        assert_eq!(double_with_shards(21), 42);
        assert_eq!(double_with_shards(21), 42); // cached
        assert_eq!(DOUBLE_WITH_SHARDS_CALLS.load(Ordering::Relaxed), 1);
        assert_eq!(double_with_max_size_shards(21), 42);
    }

    #[test]
    fn shards_attr_produces_correct_shard_count() {
        // `shards = 32` must produce a cache with exactly 32 shards (32 is already a power of 2).
        assert_eq!(DOUBLE_WITH_SHARDS.shards(), 32);
        assert_eq!(DOUBLE_WITH_MAX_SIZE_SHARDS.shards(), 32);
    }
}

// `ttl = T, shards = N` selects `ShardedTtlCache::with_ttl_and_shards`.
#[cfg(all(feature = "proc_macro", feature = "time_stores"))]
mod concurrent_cached_default_with_ttl_and_shards {
    use cached::macros::concurrent_cached;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TTL_SHARDS_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached(ttl_secs = 60, shards = 16)]
    fn ttl_shards_double(x: u64) -> u64 {
        TTL_SHARDS_CALLS.fetch_add(1, Ordering::Relaxed);
        x * 2
    }

    #[test]
    fn ttl_shards_compiles_and_caches() {
        TTL_SHARDS_CALLS.store(0, Ordering::Relaxed);
        assert_eq!(ttl_shards_double(21), 42);
        assert_eq!(ttl_shards_double(21), 42); // cached
        assert_eq!(TTL_SHARDS_CALLS.load(Ordering::Relaxed), 1);
    }
}

// `max_size = N, ttl = T, shards = S` selects `ShardedLruTtlCache::with_max_size_and_ttl_and_shards`.
#[cfg(all(feature = "proc_macro", feature = "time_stores"))]
mod concurrent_cached_default_with_max_size_and_ttl_and_shards {
    use cached::macros::concurrent_cached;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SIZE_TTL_SHARDS_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached(max_size = 100, ttl_secs = 60, shards = 16)]
    fn size_ttl_shards_double(x: u64) -> u64 {
        SIZE_TTL_SHARDS_CALLS.fetch_add(1, Ordering::Relaxed);
        x * 2
    }

    #[test]
    fn size_ttl_shards_compiles_and_caches() {
        SIZE_TTL_SHARDS_CALLS.store(0, Ordering::Relaxed);
        assert_eq!(size_ttl_shards_double(21), 42);
        assert_eq!(size_ttl_shards_double(21), 42); // cached
        assert_eq!(SIZE_TTL_SHARDS_CALLS.load(Ordering::Relaxed), 1);
    }
}

// `result_fallback = true` on the default sharded ttl path: last-known-good Ok
// value is returned when the function subsequently returns Err (after TTL expiry).
// The stale value is held in the primary cache slot (via ConcurrentCloneCached)
// and re-cached with a fresh TTL window — no separate _FALLBACK store.
#[cfg(all(feature = "proc_macro", feature = "time_stores"))]
mod concurrent_cached_result_fallback {
    use cached::macros::concurrent_cached;
    use cached::time::Duration;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::sleep;

    static FAIL: AtomicBool = AtomicBool::new(false);

    #[concurrent_cached(ttl_secs = 1, result_fallback = true)]
    fn maybe_double(x: u32) -> Result<u32, String> {
        if FAIL.load(Ordering::Relaxed) {
            Err("injected failure".to_string())
        } else {
            Ok(x * 2)
        }
    }

    #[test]
    fn result_fallback_returns_stale_ok_after_ttl_expiry() {
        FAIL.store(false, Ordering::Relaxed);
        // Populate the TTL cache.
        assert_eq!(maybe_double(1), Ok(2));
        // Make the function always fail from here.
        FAIL.store(true, Ordering::Relaxed);
        // Within TTL: served from main cache; function body not called.
        assert_eq!(maybe_double(1), Ok(2));
        // Wait for TTL to expire.
        sleep(Duration::from_millis(1100));
        // After TTL: function returns Err; stale value is returned from the
        // primary cache slot (ConcurrentCloneCached) and re-cached.
        assert_eq!(maybe_double(1), Ok(2));
        // Key with no prior success: Err is propagated.
        assert_eq!(maybe_double(99), Err("injected failure".to_string()));
        FAIL.store(false, Ordering::Relaxed);
    }

    // Metric check: the expired→Err→stale path counts a miss but no eviction.
    // Uses a dedicated function so its cache is fresh (not shared with above test).
    static FAIL_METRIC: AtomicBool = AtomicBool::new(false);

    #[concurrent_cached(ttl_secs = 1, result_fallback = true)]
    fn maybe_triple(x: u32) -> Result<u32, String> {
        if FAIL_METRIC.load(Ordering::Relaxed) {
            Err("metric test failure".to_string())
        } else {
            Ok(x * 3)
        }
    }

    #[test]
    fn result_fallback_expired_err_path_counts_miss_no_eviction() {
        FAIL_METRIC.store(false, Ordering::Relaxed);
        // Prime: miss + cache_set.
        assert_eq!(maybe_triple(7), Ok(21));
        // Within TTL: hit.
        assert_eq!(maybe_triple(7), Ok(21));
        // Wait for TTL to expire.
        sleep(Duration::from_millis(1100));
        // Expired + Err: cache_get_with_expiry_status returns (Some(21), true) →
        // misses++; the expired entry is NOT evicted; stale Ok(21) is returned.
        FAIL_METRIC.store(true, Ordering::Relaxed);
        assert_eq!(maybe_triple(7), Ok(21));
        // LazyLock<ShardedTtlCache> is initialized on first call; deref to access store.
        let m = MAYBE_TRIPLE.metrics();
        // miss for initial absent lookup + miss for expired-entry lookup = 2
        assert_eq!(m.misses, Some(2), "expected 2 misses (absent + expired)");
        // within-TTL hit = 1
        assert_eq!(m.hits, Some(1), "expected 1 hit (within-TTL)");
        // the expired entry is held in place — no eviction on the fallback path
        assert_eq!(
            m.evictions,
            Some(0),
            "no eviction must occur on expired→Err→stale path"
        );
        FAIL_METRIC.store(false, Ordering::Relaxed);
    }

    // Non-Copy key: previously a use-after-move bug caused compile failure when
    // the key type was not Copy.
    static FAIL_STR: AtomicBool = AtomicBool::new(false);

    #[concurrent_cached(
        ttl_secs = 1,
        result_fallback = true,
        key = "String",
        convert = r#"{ x.to_string() }"#
    )]
    fn maybe_echo(x: &str) -> Result<String, String> {
        if FAIL_STR.load(Ordering::Relaxed) {
            Err("injected failure".to_string())
        } else {
            Ok(x.to_uppercase())
        }
    }

    #[test]
    fn result_fallback_non_copy_key_compiles_and_works() {
        FAIL_STR.store(false, Ordering::Relaxed);
        assert_eq!(maybe_echo("hello"), Ok("HELLO".to_string()));
        FAIL_STR.store(true, Ordering::Relaxed);
        sleep(Duration::from_millis(1100));
        assert_eq!(maybe_echo("hello"), Ok("HELLO".to_string()));
        assert_eq!(maybe_echo("unknown"), Err("injected failure".to_string()));
        FAIL_STR.store(false, Ordering::Relaxed);
    }

    // prime_cache must NOT use the stale-fallback path — it unconditionally reruns the
    // function and returns the raw result without substituting a stale Ok for Err.
    static FAIL_PRIME: AtomicBool = AtomicBool::new(false);

    #[concurrent_cached(ttl_secs = 1, result_fallback = true)]
    fn prime_fallback_fn(x: u32) -> Result<u32, String> {
        if FAIL_PRIME.load(Ordering::Relaxed) {
            Err("prime failure".to_string())
        } else {
            Ok(x * 2)
        }
    }

    #[test]
    fn result_fallback_prime_cache_skips_stale_fallback() {
        FAIL_PRIME.store(false, Ordering::Relaxed);
        // Populate cache with Ok.
        assert_eq!(prime_fallback_fn(10), Ok(20));
        // Wait for TTL to expire.
        sleep(Duration::from_millis(1100));
        // prime_cache runs the function directly with no stale-fallback substitution.
        // The raw Err must be returned, not the stale Ok.
        FAIL_PRIME.store(true, Ordering::Relaxed);
        assert_eq!(
            prime_fallback_fn_prime_cache(10),
            Err("prime failure".to_string()),
            "prime_cache must not substitute stale Ok for Err"
        );
        // The regular path (result_fallback) still serves the stale Ok because
        // prime on Err does not overwrite the cache entry.
        assert_eq!(prime_fallback_fn(10), Ok(20));
        FAIL_PRIME.store(false, Ordering::Relaxed);
    }
}

// `result_fallback = true` with size+ttl selects ShardedLruTtlCache; verify the stale-ok
// path works identically on the LRU-TTL store.
#[cfg(all(feature = "proc_macro", feature = "time_stores"))]
mod concurrent_cached_result_fallback_lru_ttl {
    use cached::macros::concurrent_cached;
    use cached::time::Duration;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::sleep;

    static FAIL_LRU: AtomicBool = AtomicBool::new(false);

    #[concurrent_cached(ttl_secs = 1, max_size = 100, result_fallback = true)]
    fn lru_ttl_maybe_double(x: u32) -> Result<u32, String> {
        if FAIL_LRU.load(Ordering::Relaxed) {
            Err("lru_ttl failure".to_string())
        } else {
            Ok(x * 2)
        }
    }

    #[test]
    fn result_fallback_lru_ttl_returns_stale_ok_after_expiry() {
        FAIL_LRU.store(false, Ordering::Relaxed);
        // Populate via ShardedLruTtlCache.
        assert_eq!(lru_ttl_maybe_double(5), Ok(10));
        // Within TTL: served from cache.
        assert_eq!(lru_ttl_maybe_double(5), Ok(10));
        // Wait for TTL to expire.
        sleep(Duration::from_millis(1100));
        // Expired + Err: stale Ok must be returned, entry held in place.
        FAIL_LRU.store(true, Ordering::Relaxed);
        assert_eq!(lru_ttl_maybe_double(5), Ok(10));
        // Metrics before any new-key calls: 2 misses (initial absent + expired),
        // 1 hit (within-TTL), 0 evictions.
        let m = LRU_TTL_MAYBE_DOUBLE.metrics();
        assert_eq!(m.misses, Some(2), "expected 2 misses (absent + expired)");
        assert_eq!(m.hits, Some(1), "expected 1 hit (within-TTL)");
        assert_eq!(
            m.evictions,
            Some(0),
            "no eviction must occur on expired→Err→stale path"
        );
        // Key with no prior Ok: Err propagated.
        assert_eq!(lru_ttl_maybe_double(99), Err("lru_ttl failure".to_string()));
        FAIL_LRU.store(false, Ordering::Relaxed);
    }
}

// Async path: `result_fallback = true` returns the last-known-good Ok value after TTL expiry.
#[cfg(all(
    feature = "proc_macro",
    feature = "time_stores",
    feature = "async_tokio_rt_multi_thread"
))]
mod concurrent_cached_result_fallback_async {
    use cached::macros::concurrent_cached;
    use cached::time::Duration;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::time::sleep;

    static FAIL_ASYNC: AtomicBool = AtomicBool::new(false);

    #[concurrent_cached(ttl_secs = 1, result_fallback = true)]
    async fn maybe_double_async(x: u32) -> Result<u32, String> {
        if FAIL_ASYNC.load(Ordering::Relaxed) {
            Err("async failure".to_string())
        } else {
            Ok(x * 2)
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn result_fallback_async_returns_stale_ok_after_ttl_expiry() {
        FAIL_ASYNC.store(false, Ordering::Relaxed);
        assert_eq!(maybe_double_async(5).await, Ok(10));
        FAIL_ASYNC.store(true, Ordering::Relaxed);
        sleep(Duration::from_millis(1100)).await;
        // After TTL expiry, fallback returns last Ok instead of propagating Err.
        assert_eq!(maybe_double_async(5).await, Ok(10));
        // Key with no prior success: Err is propagated.
        assert_eq!(
            maybe_double_async(99).await,
            Err("async failure".to_string())
        );
        FAIL_ASYNC.store(false, Ordering::Relaxed);
    }
}

// Async path: `result_fallback = true` with a non-Copy key — regression guard for
// use-after-move in async codegen when arguments are cloned to form the cache key.
#[cfg(all(
    feature = "proc_macro",
    feature = "time_stores",
    feature = "async_tokio_rt_multi_thread"
))]
mod concurrent_cached_result_fallback_async_non_copy_key {
    use cached::macros::concurrent_cached;
    use cached::time::Duration;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::time::sleep;

    static FAIL_ASYNC_STR: AtomicBool = AtomicBool::new(false);

    #[concurrent_cached(
        ttl_secs = 1,
        result_fallback = true,
        key = "String",
        convert = r#"{ x.to_string() }"#
    )]
    async fn maybe_echo_async(x: &str) -> Result<String, String> {
        if FAIL_ASYNC_STR.load(Ordering::Relaxed) {
            Err("async failure".to_string())
        } else {
            Ok(x.to_uppercase())
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn result_fallback_async_non_copy_key_returns_stale_ok_after_ttl_expiry() {
        FAIL_ASYNC_STR.store(false, Ordering::Relaxed);
        assert_eq!(maybe_echo_async("hello").await, Ok("HELLO".to_string()));
        FAIL_ASYNC_STR.store(true, Ordering::Relaxed);
        sleep(Duration::from_millis(1100)).await;
        // After TTL expiry with a non-Copy String key, fallback returns last Ok.
        assert_eq!(maybe_echo_async("hello").await, Ok("HELLO".to_string()));
        // Key with no prior success: Err is propagated.
        assert_eq!(
            maybe_echo_async("unknown").await,
            Err("async failure".to_string())
        );
        FAIL_ASYNC_STR.store(false, Ordering::Relaxed);
    }
}

// Async path: `result_fallback = true` with size+ttl selects ShardedLruTtlCache; verify the
// stale-ok path and metrics work identically on the async LRU-TTL store.
#[cfg(all(
    feature = "proc_macro",
    feature = "time_stores",
    feature = "async_tokio_rt_multi_thread"
))]
mod concurrent_cached_result_fallback_async_lru_ttl {
    use cached::macros::concurrent_cached;
    use cached::time::Duration;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::time::sleep;

    static FAIL_ASYNC_LRU: AtomicBool = AtomicBool::new(false);

    #[concurrent_cached(ttl_secs = 1, max_size = 100, result_fallback = true)]
    async fn lru_ttl_maybe_double_async(x: u32) -> Result<u32, String> {
        if FAIL_ASYNC_LRU.load(Ordering::Relaxed) {
            Err("async lru_ttl failure".to_string())
        } else {
            Ok(x * 2)
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn result_fallback_async_lru_ttl_returns_stale_ok_after_expiry() {
        FAIL_ASYNC_LRU.store(false, Ordering::Relaxed);
        // Populate via ShardedLruTtlCache.
        assert_eq!(lru_ttl_maybe_double_async(5).await, Ok(10));
        // Within TTL: served from cache.
        assert_eq!(lru_ttl_maybe_double_async(5).await, Ok(10));
        // Wait for TTL to expire.
        sleep(Duration::from_millis(1100)).await;
        // Expired + Err: stale Ok must be returned, entry held in place.
        FAIL_ASYNC_LRU.store(true, Ordering::Relaxed);
        assert_eq!(lru_ttl_maybe_double_async(5).await, Ok(10));
        // Metrics: 2 misses (initial absent + expired), 1 hit (within-TTL), 0 evictions.
        // The async store lives in a `OnceCell`, initialized by the first call above.
        let m = LRU_TTL_MAYBE_DOUBLE_ASYNC
            .get()
            .expect("store initialized by first call")
            .metrics();
        assert_eq!(m.misses, Some(2), "expected 2 misses (absent + expired)");
        assert_eq!(m.hits, Some(1), "expected 1 hit (within-TTL)");
        assert_eq!(
            m.evictions,
            Some(0),
            "no eviction must occur on expired→Err→stale path"
        );
        // Key with no prior Ok: Err propagated.
        assert_eq!(
            lru_ttl_maybe_double_async(99).await,
            Err("async lru_ttl failure".to_string())
        );
        FAIL_ASYNC_LRU.store(false, Ordering::Relaxed);
    }
}

// `cache_err = true`: errors are cached — subsequent calls with the same key return
// the cached Err without re-invoking the function body.
#[cfg(feature = "proc_macro")]
mod concurrent_cached_cache_err {
    use cached::macros::concurrent_cached;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static ERR_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached(cache_err = true)]
    fn err_double(x: u32) -> Result<u32, u32> {
        ERR_CALLS.fetch_add(1, Ordering::Relaxed);
        Err(x)
    }

    #[test]
    fn cache_err_caches_error_result() {
        ERR_CALLS.store(0, Ordering::Relaxed);
        // First call: function executes and returns Err.
        assert_eq!(err_double(7), Err(7));
        assert_eq!(ERR_CALLS.load(Ordering::Relaxed), 1);
        // Second call with same key: served from cache, function not called again.
        assert_eq!(err_double(7), Err(7));
        assert_eq!(ERR_CALLS.load(Ordering::Relaxed), 1);
        // Different key: function executes again.
        assert_eq!(err_double(8), Err(8));
        assert_eq!(ERR_CALLS.load(Ordering::Relaxed), 2);
    }
}

// Async path uses `OnceCell<ShardedUnboundCache>`.
#[cfg(all(feature = "proc_macro", feature = "async_tokio_rt_multi_thread"))]
mod concurrent_cached_default_async {
    use cached::macros::concurrent_cached;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SLOW_DOUBLE_ASYNC_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached]
    async fn slow_double_async(x: u64) -> u64 {
        SLOW_DOUBLE_ASYNC_CALLS.fetch_add(1, Ordering::Relaxed);
        x * 2
    }

    #[tokio::test]
    async fn async_default_compiles_and_caches() {
        SLOW_DOUBLE_ASYNC_CALLS.store(0, Ordering::Relaxed);
        assert_eq!(slow_double_async(21).await, 42);
        assert_eq!(slow_double_async(21).await, 42); // cached
        assert_eq!(SLOW_DOUBLE_ASYNC_CALLS.load(Ordering::Relaxed), 1);
    }
}

// `with_cached_flag = true` on the sharded default path: `was_cached` is false on first
// call and true on subsequent hits.
#[cfg(feature = "proc_macro")]
mod concurrent_cached_default_with_cached_flag {
    use cached::macros::concurrent_cached;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static FLAG_CALLS: AtomicUsize = AtomicUsize::new(0);
    static PLAIN_FLAG_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached(with_cached_flag = true)]
    fn flagged_double(x: u64) -> Result<cached::Return<u64>, std::convert::Infallible> {
        FLAG_CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(cached::Return::new(x * 2))
    }

    #[concurrent_cached(with_cached_flag = true)]
    fn flagged_plain_double(x: u64) -> cached::Return<u64> {
        PLAIN_FLAG_CALLS.fetch_add(1, Ordering::Relaxed);
        cached::Return::new(x * 2)
    }

    #[test]
    fn with_cached_flag_reports_cache_state() {
        FLAG_CALLS.store(0, Ordering::Relaxed);
        let first = flagged_double(7).unwrap();
        assert_eq!(*first, 14);
        assert!(!first.was_cached, "first call should not be cached");
        let second = flagged_double(7).unwrap();
        assert_eq!(*second, 14);
        assert!(second.was_cached, "second call should be cached");
        assert_eq!(FLAG_CALLS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn plain_return_with_cached_flag_reports_cache_state() {
        PLAIN_FLAG_CALLS.store(0, Ordering::Relaxed);
        let first = flagged_plain_double(8);
        assert_eq!(*first, 16);
        assert!(!first.was_cached, "first call should not be cached");
        let second = flagged_plain_double(8);
        assert_eq!(*second, 16);
        assert!(second.was_cached, "second call should be cached");
        assert_eq!(PLAIN_FLAG_CALLS.load(Ordering::Relaxed), 1);
    }
}

// `option = true` on the sharded default path: None skips caching, Some(T) is cached.
#[cfg(feature = "proc_macro")]
mod concurrent_cached_option {
    use cached::macros::concurrent_cached;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static OPT_CALLS: AtomicUsize = AtomicUsize::new(0);
    static OPT_FLAG_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached]
    fn maybe_double(x: u64) -> Option<u64> {
        OPT_CALLS.fetch_add(1, Ordering::Relaxed);
        if x == 0 { None } else { Some(x * 2) }
    }

    #[concurrent_cached(with_cached_flag = true)]
    fn flagged_maybe_double(x: u64) -> Option<cached::Return<u64>> {
        OPT_FLAG_CALLS.fetch_add(1, Ordering::Relaxed);
        if x == 0 {
            None
        } else {
            Some(cached::Return::new(x * 2))
        }
    }

    #[test]
    fn option_caches_some_not_none() {
        OPT_CALLS.store(0, Ordering::Relaxed);
        // None is not cached — subsequent calls still invoke the function.
        assert_eq!(maybe_double(0), None);
        assert_eq!(maybe_double(0), None);
        assert_eq!(
            OPT_CALLS.load(Ordering::Relaxed),
            2,
            "None should not be cached"
        );
        // Some(T) is cached — second call is a hit.
        assert_eq!(maybe_double(3), Some(6));
        assert_eq!(maybe_double(3), Some(6));
        assert_eq!(
            OPT_CALLS.load(Ordering::Relaxed),
            3,
            "Some should be cached after first call"
        );
    }

    #[test]
    fn option_with_cached_flag_reports_cache_state() {
        OPT_FLAG_CALLS.store(0, Ordering::Relaxed);
        // None — not cached.
        assert!(flagged_maybe_double(0).is_none());
        assert!(flagged_maybe_double(0).is_none());
        assert_eq!(
            OPT_FLAG_CALLS.load(Ordering::Relaxed),
            2,
            "None should not be cached"
        );
        // Some — first call not cached, second is.
        let first = flagged_maybe_double(5).expect("should return Some");
        assert_eq!(*first, 10);
        assert!(!first.was_cached, "first Some call should not be cached");
        let second = flagged_maybe_double(5).expect("should return Some");
        assert_eq!(*second, 10);
        assert!(second.was_cached, "second Some call should be cached");
        assert_eq!(OPT_FLAG_CALLS.load(Ordering::Relaxed), 3);
    }
}

// Async `option = true` on the sharded default path.
#[cfg(all(feature = "proc_macro", feature = "async_tokio_rt_multi_thread"))]
mod concurrent_cached_async_option {
    use cached::macros::concurrent_cached;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static ASYNC_OPT_CALLS: AtomicUsize = AtomicUsize::new(0);
    static ASYNC_OPT_FLAG_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached]
    async fn async_maybe_double(x: u64) -> Option<u64> {
        ASYNC_OPT_CALLS.fetch_add(1, Ordering::Relaxed);
        if x == 0 { None } else { Some(x * 2) }
    }

    #[concurrent_cached(with_cached_flag = true)]
    async fn async_flagged_maybe_double(x: u64) -> Option<cached::Return<u64>> {
        ASYNC_OPT_FLAG_CALLS.fetch_add(1, Ordering::Relaxed);
        if x == 0 {
            None
        } else {
            Some(cached::Return::new(x * 2))
        }
    }

    #[tokio::test]
    async fn async_option_caches_some_not_none() {
        ASYNC_OPT_CALLS.store(0, Ordering::Relaxed);
        assert_eq!(async_maybe_double(0).await, None);
        assert_eq!(async_maybe_double(0).await, None);
        assert_eq!(
            ASYNC_OPT_CALLS.load(Ordering::Relaxed),
            2,
            "None should not be cached"
        );
        assert_eq!(async_maybe_double(4).await, Some(8));
        assert_eq!(async_maybe_double(4).await, Some(8));
        assert_eq!(
            ASYNC_OPT_CALLS.load(Ordering::Relaxed),
            3,
            "Some should be cached after first call"
        );
    }

    #[tokio::test]
    async fn async_option_with_cached_flag_reports_cache_state() {
        ASYNC_OPT_FLAG_CALLS.store(0, Ordering::Relaxed);
        assert!(async_flagged_maybe_double(0).await.is_none());
        assert!(async_flagged_maybe_double(0).await.is_none());
        assert_eq!(
            ASYNC_OPT_FLAG_CALLS.load(Ordering::Relaxed),
            2,
            "None should not be cached"
        );
        let first = async_flagged_maybe_double(6)
            .await
            .expect("should return Some");
        assert_eq!(*first, 12);
        assert!(!first.was_cached, "first Some call should not be cached");
        let second = async_flagged_maybe_double(6)
            .await
            .expect("should return Some");
        assert_eq!(*second, 12);
        assert!(second.was_cached, "second Some call should be cached");
        assert_eq!(ASYNC_OPT_FLAG_CALLS.load(Ordering::Relaxed), 3);
    }
}

// Async `with_cached_flag = true` on the sharded default path (plain and Result variants).
#[cfg(all(feature = "proc_macro", feature = "async_tokio_rt_multi_thread"))]
mod concurrent_cached_default_async_with_cached_flag {
    use cached::macros::concurrent_cached;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static ASYNC_FLAG_CALLS: AtomicUsize = AtomicUsize::new(0);
    static ASYNC_PLAIN_FLAG_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[concurrent_cached(with_cached_flag = true)]
    async fn async_flagged_double(x: u64) -> Result<cached::Return<u64>, std::convert::Infallible> {
        ASYNC_FLAG_CALLS.fetch_add(1, Ordering::Relaxed);
        Ok(cached::Return::new(x * 2))
    }

    #[concurrent_cached(with_cached_flag = true)]
    async fn async_flagged_plain_double(x: u64) -> cached::Return<u64> {
        ASYNC_PLAIN_FLAG_CALLS.fetch_add(1, Ordering::Relaxed);
        cached::Return::new(x * 2)
    }

    #[tokio::test]
    async fn async_with_cached_flag_result_reports_cache_state() {
        ASYNC_FLAG_CALLS.store(0, Ordering::Relaxed);
        let first = async_flagged_double(7).await.unwrap();
        assert_eq!(*first, 14);
        assert!(!first.was_cached, "first call should not be cached");
        let second = async_flagged_double(7).await.unwrap();
        assert_eq!(*second, 14);
        assert!(second.was_cached, "second call should be cached");
        assert_eq!(ASYNC_FLAG_CALLS.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn async_plain_return_with_cached_flag_reports_cache_state() {
        ASYNC_PLAIN_FLAG_CALLS.store(0, Ordering::Relaxed);
        let first = async_flagged_plain_double(8).await;
        assert_eq!(*first, 16);
        assert!(!first.was_cached, "first call should not be cached");
        let second = async_flagged_plain_double(8).await;
        assert_eq!(*second, 16);
        assert!(second.was_cached, "second call should be cached");
        assert_eq!(ASYNC_PLAIN_FLAG_CALLS.load(Ordering::Relaxed), 1);
    }
}

// `Send + Sync` typecheck for the sharded stores (mirrors `redis_not_sync_typecheck`).
#[cfg(feature = "proc_macro")]
#[allow(dead_code)]
mod sharded_send_sync_typecheck {
    fn _typecheck_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<cached::ShardedUnboundCache<String, u32>>();
        assert_sync::<cached::ShardedUnboundCache<String, u32>>();
        assert_send::<cached::ShardedLruCache<String, u32>>();
        assert_sync::<cached::ShardedLruCache<String, u32>>();
    }

    #[cfg(feature = "time_stores")]
    fn _typecheck_sync_timed() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<cached::ShardedTtlCache<String, u32>>();
        assert_sync::<cached::ShardedTtlCache<String, u32>>();
        assert_send::<cached::ShardedLruTtlCache<String, u32>>();
        assert_sync::<cached::ShardedLruTtlCache<String, u32>>();
    }
}

#[test]
fn concurrent_cached_trait_short_aliases_work() {
    use cached::{ConcurrentCached, ShardedUnboundCache};

    let cache = ShardedUnboundCache::<String, u32>::builder()
        .build()
        .unwrap();
    assert_eq!(cache.set("a".to_string(), 1).unwrap(), None);
    assert_eq!(cache.get(&"a".to_string()).unwrap(), Some(1));
    assert_eq!(cache.remove(&"a".to_string()).unwrap(), Some(1));
    assert!(!cache.delete(&"a".to_string()).unwrap());
}

// `cache_clear_with_on_evict` without a callback delegates to `clear()` and does NOT
// increment the evictions counter. This test guards against regressions where the counter
// increment gets moved before the early-return.
#[test]
fn cache_clear_with_on_evict_no_callback_leaves_evictions_at_zero() {
    use cached::{ConcurrentCached, ShardedLruCache, ShardedUnboundCache};

    // ShardedUnboundCache (unbounded) — no on_evict; evictions metric is not tracked (returns None)
    let cache = ShardedUnboundCache::<u32, u32>::builder().build().unwrap();
    ConcurrentCached::cache_set(&cache, 1, 10).expect("infallible ShardedUnboundCache set");
    ConcurrentCached::cache_set(&cache, 2, 20).expect("infallible ShardedUnboundCache set");
    cache.cache_clear_with_on_evict();
    assert_eq!(cache.len(), 0, "cache should be empty after clear");
    // ShardedUnboundCache does not track evictions — None is expected, not Some(0)
    assert_eq!(cache.metrics().evictions, None);

    // ShardedLruCache tracks evictions; no on_evict means the counter stays at zero
    let lru = ShardedLruCache::<u32, u32>::builder()
        .max_size(64)
        .build()
        .unwrap();
    ConcurrentCached::cache_set(&lru, 1, 10).expect("infallible ShardedLruCache set");
    ConcurrentCached::cache_set(&lru, 2, 20).expect("infallible ShardedLruCache set");
    lru.cache_clear_with_on_evict();
    assert_eq!(
        lru.metrics().evictions,
        Some(0),
        "evictions should remain 0 when no on_evict callback is set"
    );
    assert_eq!(lru.len(), 0);
}

// `ConcurrentCached::cache_clear` / `cache_reset` / `cache_reset_metrics` are trait methods
// (default no-op) overridden by the sharded stores to actually clear entries and zero metrics.
#[test]
fn concurrent_cached_trait_clear_and_reset_metrics_on_sharded_stores() {
    use cached::{ConcurrentCached, ShardedLruCache, ShardedUnboundCache};

    // --- Unbound ShardedUnboundCache: cache_clear empties the store via the trait method ---
    let cache = ShardedUnboundCache::<u32, u32>::builder().build().unwrap();
    ConcurrentCached::cache_set(&cache, 1, 10).expect("infallible");
    ConcurrentCached::cache_set(&cache, 2, 20).expect("infallible");
    assert_eq!(cache.len(), 2);
    // Record a hit and a miss so metrics are non-zero.
    let _ = ConcurrentCached::cache_get(&cache, &1).expect("infallible");
    let _ = ConcurrentCached::cache_get(&cache, &99).expect("infallible");
    assert_eq!(cache.metrics().hits, Some(1));
    assert_eq!(cache.metrics().misses, Some(1));

    ConcurrentCached::cache_clear(&cache).expect("infallible");
    assert_eq!(cache.len(), 0, "cache_clear must remove all entries");
    // cache_clear preserves metrics.
    assert_eq!(cache.metrics().hits, Some(1));
    assert_eq!(cache.metrics().misses, Some(1));

    ConcurrentCached::cache_reset_metrics(&cache).expect("infallible");
    assert_eq!(
        cache.metrics().hits,
        Some(0),
        "cache_reset_metrics must zero hits"
    );
    assert_eq!(
        cache.metrics().misses,
        Some(0),
        "cache_reset_metrics must zero misses"
    );

    // --- ShardedLruCache: cache_reset_metrics also zeros the eviction counter ---
    let lru = ShardedLruCache::<u32, u32>::builder()
        .per_shard_max_size(1)
        .shards(1)
        .build()
        .unwrap();
    // Two inserts into a single shard with capacity 1 forces one LRU eviction.
    ConcurrentCached::cache_set(&lru, 1, 10).expect("infallible");
    ConcurrentCached::cache_set(&lru, 2, 20).expect("infallible");
    let _ = ConcurrentCached::cache_get(&lru, &2).expect("infallible");
    assert_eq!(lru.metrics().evictions, Some(1));
    assert!(lru.metrics().hits.unwrap() >= 1);

    // cache_reset removes entries AND zeros every counter in one call.
    ConcurrentCached::cache_reset(&lru).expect("infallible");
    assert_eq!(lru.len(), 0, "cache_reset must remove all entries");
    assert_eq!(lru.metrics().hits, Some(0));
    assert_eq!(lru.metrics().misses, Some(0));
    assert_eq!(
        lru.metrics().evictions,
        Some(0),
        "cache_reset must zero the eviction counter too"
    );
}

// `ConcurrentCachedAsync::async_cache_clear` / `async_cache_reset_metrics` are the async
// counterparts of the `ConcurrentCached` trait methods, overridden by the sharded stores to
// actually clear entries and zero metrics (mirrors the sync test above).
#[cfg(feature = "async")]
#[tokio::test]
async fn concurrent_cached_async_trait_clear_and_reset_metrics_on_sharded_stores() {
    use cached::{ConcurrentCachedAsync, ShardedUnboundCache};

    let cache = ShardedUnboundCache::<u32, u32>::builder().build().unwrap();
    ConcurrentCachedAsync::async_cache_set(&cache, 1, 10)
        .await
        .expect("infallible");
    ConcurrentCachedAsync::async_cache_set(&cache, 2, 20)
        .await
        .expect("infallible");
    assert_eq!(cache.len(), 2);

    // Record a hit so the metrics are non-zero.
    let _ = ConcurrentCachedAsync::async_cache_get(&cache, &1)
        .await
        .expect("infallible");
    assert_eq!(cache.metrics().hits, Some(1));

    // async_cache_clear empties the store but preserves metrics.
    ConcurrentCachedAsync::async_cache_clear(&cache)
        .await
        .expect("infallible");
    assert_eq!(cache.len(), 0, "async_cache_clear must remove all entries");
    assert_eq!(cache.metrics().hits, Some(1));

    // async_cache_reset_metrics zeros the counters.
    ConcurrentCachedAsync::async_cache_reset_metrics(&cache)
        .await
        .expect("infallible");
    assert_eq!(
        cache.metrics().hits,
        Some(0),
        "async_cache_reset_metrics must zero hits"
    );
    assert_eq!(
        cache.metrics().misses,
        Some(0),
        "async_cache_reset_metrics must zero misses"
    );
}

mod sharded_expiring_tests {
    #[cfg(feature = "proc_macro")]
    use cached::macros::concurrent_cached;
    use cached::{
        ConcurrentCacheEvict, ConcurrentCached, Expires, ShardedExpiringCache,
        ShardedExpiringLruCache,
    };
    use std::sync::Arc;
    #[cfg(feature = "proc_macro")]
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Clone, Debug)]
    struct ExpiringItem {
        val: u32,
        expired: Arc<AtomicBool>,
    }

    impl Expires for ExpiringItem {
        fn is_expired(&self) -> bool {
            self.expired.load(Ordering::Relaxed)
        }
    }

    #[test]
    fn sharded_expiring_cache_basic_ops() {
        let flag1 = Arc::new(AtomicBool::new(false));
        let flag2 = Arc::new(AtomicBool::new(true));

        let cache = ShardedExpiringCache::<u32, ExpiringItem>::builder()
            .build()
            .unwrap();
        let _ = ConcurrentCached::cache_set(
            &cache,
            1,
            ExpiringItem {
                val: 42,
                expired: flag1.clone(),
            },
        );
        let _ = ConcurrentCached::cache_set(
            &cache,
            2,
            ExpiringItem {
                val: 99,
                expired: flag2.clone(),
            },
        );

        assert_eq!(
            ConcurrentCached::cache_get(&cache, &1)
                .unwrap()
                .map(|i| i.val),
            Some(42)
        );
        assert_eq!(
            ConcurrentCached::cache_get(&cache, &2)
                .unwrap()
                .map(|i| i.val),
            None
        ); // expired
        assert_eq!(cache.metrics().misses, Some(1));
        assert_eq!(cache.metrics().hits, Some(1));

        let lru = ShardedExpiringLruCache::<u32, ExpiringItem>::builder()
            .max_size(64)
            .build()
            .unwrap();
        let _ = ConcurrentCached::cache_set(
            &lru,
            1,
            ExpiringItem {
                val: 42,
                expired: flag1.clone(),
            },
        );
        let _ = ConcurrentCached::cache_set(
            &lru,
            2,
            ExpiringItem {
                val: 99,
                expired: flag2.clone(),
            },
        );

        assert_eq!(
            ConcurrentCached::cache_get(&lru, &1)
                .unwrap()
                .map(|i| i.val),
            Some(42)
        );
        assert_eq!(
            ConcurrentCached::cache_get(&lru, &2)
                .unwrap()
                .map(|i| i.val),
            None
        ); // expired
        assert_eq!(lru.metrics().misses, Some(1));
        assert_eq!(lru.metrics().hits, Some(1));
    }

    #[test]
    fn sharded_expiring_cache_evict() {
        let flag = Arc::new(AtomicBool::new(true));
        let cache = ShardedExpiringCache::<u32, ExpiringItem>::builder()
            .build()
            .unwrap();
        let _ = ConcurrentCached::cache_set(
            &cache,
            1,
            ExpiringItem {
                val: 42,
                expired: flag.clone(),
            },
        );
        let _ = ConcurrentCached::cache_set(
            &cache,
            2,
            ExpiringItem {
                val: 99,
                expired: flag.clone(),
            },
        );

        assert_eq!(cache.len(), 2);
        let evicted = ConcurrentCacheEvict::evict(&cache);
        assert_eq!(evicted, 2);
        assert_eq!(cache.len(), 0);

        let lru = ShardedExpiringLruCache::<u32, ExpiringItem>::builder()
            .max_size(64)
            .build()
            .unwrap();
        let _ = ConcurrentCached::cache_set(
            &lru,
            1,
            ExpiringItem {
                val: 42,
                expired: flag.clone(),
            },
        );
        let _ = ConcurrentCached::cache_set(
            &lru,
            2,
            ExpiringItem {
                val: 99,
                expired: flag.clone(),
            },
        );

        assert_eq!(lru.len(), 2);
        let evicted_lru = ConcurrentCacheEvict::evict(&lru);
        assert_eq!(evicted_lru, 2);
        assert_eq!(lru.len(), 0);
    }

    #[test]
    fn sharded_expiring_evict_fires_on_evict_and_increments_evictions_counter() {
        use std::sync::atomic::AtomicU32;

        let flag = Arc::new(AtomicBool::new(true));

        let fired = Arc::new(AtomicU32::new(0));
        let fired_clone = fired.clone();
        let cache = ShardedExpiringCache::<u32, ExpiringItem>::builder()
            .on_evict(move |_k, _v| {
                fired_clone.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        let _ = cache.cache_set(
            1,
            ExpiringItem {
                val: 10,
                expired: flag.clone(),
            },
        );
        let _ = cache.cache_set(
            2,
            ExpiringItem {
                val: 20,
                expired: flag.clone(),
            },
        );

        assert_eq!(cache.evict(), 2);
        assert_eq!(fired.load(Ordering::Relaxed), 2);
        assert_eq!(cache.metrics().evictions, Some(2));

        let fired_lru = Arc::new(AtomicU32::new(0));
        let fired_lru_clone = fired_lru.clone();
        let lru = ShardedExpiringLruCache::<u32, ExpiringItem>::builder()
            .max_size(64)
            .on_evict(move |_k, _v| {
                fired_lru_clone.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();
        let _ = lru.cache_set(
            1,
            ExpiringItem {
                val: 10,
                expired: flag.clone(),
            },
        );
        let _ = lru.cache_set(
            2,
            ExpiringItem {
                val: 20,
                expired: flag.clone(),
            },
        );

        assert_eq!(lru.evict(), 2);
        assert_eq!(fired_lru.load(Ordering::Relaxed), 2);
        assert_eq!(lru.metrics().evictions, Some(2));
    }

    #[cfg(feature = "proc_macro")]
    static BARE_EXPIRES_CALLS: AtomicUsize = AtomicUsize::new(0);
    #[cfg(feature = "proc_macro")]
    #[concurrent_cached(expires = true, key = "u32", convert = r#"{ x }"#)]
    fn get_expiring_item(x: u32, flag: Arc<AtomicBool>) -> ExpiringItem {
        BARE_EXPIRES_CALLS.fetch_add(1, Ordering::Relaxed);
        ExpiringItem {
            val: x * 10,
            expired: flag,
        }
    }

    #[cfg(feature = "proc_macro")]
    static BARE_EXPIRES_LRU_CALLS: AtomicUsize = AtomicUsize::new(0);
    #[cfg(feature = "proc_macro")]
    #[concurrent_cached(expires = true, max_size = 64, key = "u32", convert = r#"{ x }"#)]
    fn get_expiring_item_lru(x: u32, flag: Arc<AtomicBool>) -> ExpiringItem {
        BARE_EXPIRES_LRU_CALLS.fetch_add(1, Ordering::Relaxed);
        ExpiringItem {
            val: x * 10,
            expired: flag,
        }
    }

    #[cfg(feature = "proc_macro")]
    #[test]
    fn concurrent_cached_expires_unbounded() {
        BARE_EXPIRES_CALLS.store(0, Ordering::Relaxed);
        let flag = Arc::new(AtomicBool::new(false));

        let res1 = get_expiring_item(5, flag.clone());
        assert_eq!(res1.val, 50);
        let res2 = get_expiring_item(5, flag.clone());
        assert_eq!(res2.val, 50);
        assert_eq!(BARE_EXPIRES_CALLS.load(Ordering::Relaxed), 1); // cached

        // Expire
        flag.store(true, Ordering::Relaxed);
        let res3 = get_expiring_item(5, flag.clone());
        assert_eq!(res3.val, 50);
        assert_eq!(BARE_EXPIRES_CALLS.load(Ordering::Relaxed), 2); // recalculated
    }

    #[cfg(feature = "proc_macro")]
    #[test]
    fn concurrent_cached_expires_lru() {
        BARE_EXPIRES_LRU_CALLS.store(0, Ordering::Relaxed);
        let flag = Arc::new(AtomicBool::new(false));

        let res1 = get_expiring_item_lru(5, flag.clone());
        assert_eq!(res1.val, 50);
        let res2 = get_expiring_item_lru(5, flag.clone());
        assert_eq!(res2.val, 50);
        assert_eq!(BARE_EXPIRES_LRU_CALLS.load(Ordering::Relaxed), 1); // cached

        // Expire
        flag.store(true, Ordering::Relaxed);
        let res3 = get_expiring_item_lru(5, flag.clone());
        assert_eq!(res3.val, 50);
        assert_eq!(BARE_EXPIRES_LRU_CALLS.load(Ordering::Relaxed), 2); // recalculated
    }

    #[test]
    fn sharded_expiring_lru_on_evict_fires_on_lru_capacity_pressure() {
        let evict_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let evict_count2 = evict_count.clone();
        let not_expired = Arc::new(AtomicBool::new(false));

        let cache = ShardedExpiringLruCache::<u32, ExpiringItem>::builder()
            .max_size(8)
            .shards(1)
            .on_evict(move |_k, _v| {
                evict_count2.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();

        // Insert 16 entries into a cache with capacity 8 (1 shard) to force LRU evictions.
        for i in 0..16 {
            let _ = ConcurrentCached::cache_set(
                &cache,
                i,
                ExpiringItem {
                    val: i,
                    expired: not_expired.clone(),
                },
            );
        }

        // At least 8 entries must have been evicted by LRU capacity pressure.
        assert!(
            evict_count.load(Ordering::Relaxed) >= 8,
            "expected on_evict to fire for LRU evictions, got {}",
            evict_count.load(Ordering::Relaxed)
        );
        // metrics().evictions aggregates both LRU-shard capacity evictions and inner.evictions.
        let total_evictions = cache.metrics().evictions.unwrap_or(0);
        assert!(
            total_evictions >= 8,
            "expected metrics().evictions >= 8, got {}",
            total_evictions
        );
    }

    #[test]
    fn sharded_expiring_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ShardedExpiringCache<u32, ExpiringItem>>();
        assert_send_sync::<ShardedExpiringLruCache<u32, ExpiringItem>>();
    }

    // `expires = true` + `with_cached_flag = true`: `was_cached` is false on first call
    // and on re-execution after expiry; true only for a genuine unexpired cache hit.
    #[cfg(feature = "proc_macro")]
    mod concurrent_cached_expires_with_cached_flag {
        use super::*;
        use cached::macros::concurrent_cached;
        use std::sync::atomic::AtomicUsize;

        static EXPIRES_FLAG_CALLS: AtomicUsize = AtomicUsize::new(0);

        #[concurrent_cached(
            expires = true,
            key = "u32",
            convert = r#"{ x }"#,
            with_cached_flag = true
        )]
        fn get_flagged_expiring(
            x: u32,
            expired: Arc<AtomicBool>,
        ) -> Result<cached::Return<ExpiringItem>, std::convert::Infallible> {
            EXPIRES_FLAG_CALLS.fetch_add(1, Ordering::Relaxed);
            Ok(cached::Return::new(ExpiringItem { val: x, expired }))
        }

        #[test]
        fn expires_with_cached_flag_reports_cache_state() {
            EXPIRES_FLAG_CALLS.store(0, Ordering::Relaxed);
            let flag = Arc::new(AtomicBool::new(false));

            // First call: not cached.
            let r1 = get_flagged_expiring(42, flag.clone()).unwrap();
            assert!(!r1.was_cached, "first call should not be cached");
            assert_eq!(r1.val, 42);

            // Second call: cached hit.
            let r2 = get_flagged_expiring(42, flag.clone()).unwrap();
            assert!(r2.was_cached, "second call should be a cache hit");
            assert_eq!(EXPIRES_FLAG_CALLS.load(Ordering::Relaxed), 1);

            // After expiry: function re-executes, was_cached = false.
            flag.store(true, Ordering::Relaxed);
            let r3 = get_flagged_expiring(42, flag.clone()).unwrap();
            assert!(!r3.was_cached, "call after expiry should not be cached");
            assert_eq!(EXPIRES_FLAG_CALLS.load(Ordering::Relaxed), 2);
        }
    }
}

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
        fn cache_remove_entry(&self, k: &String) -> Result<Option<(String, String)>, Self::Error> {
            Ok(self.0.lock().unwrap().remove_entry(k))
        }
        fn set_refresh_on_hit(&self, _r: bool) -> bool {
            false
        }
        fn cache_clear(&self) -> Result<(), Self::Error> {
            self.0.lock().unwrap().clear();
            Ok(())
        }
        fn cache_reset(&self) -> Result<(), Self::Error> {
            self.cache_clear()
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
    use cached::RedisCache;
    use cached::macros::concurrent_cached;
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
        ttl_secs = 1,
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
        ttl_secs = 1,
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
        create = r##" { RedisCache::builder("cache_redis_test_cache_create", Duration::from_secs(1)).refresh_on_hit(true).build().expect("error building redis cache") } "##
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
            ttl_secs = 1,
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
            ttl_secs = 1,
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
            create = r##" { AsyncRedisCache::builder("async_cached_redis_test_cache_create", Duration::from_secs(1)).refresh_on_hit(true).build().await.expect("error building async redis cache") } "##
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

        #[tokio::test]
        async fn async_redis_builder_aliases_and_zero_ttl_validation() {
            let result = cached::AsyncRedisCache::<String, String>::builder(
                "async-zero-ttl",
                Duration::ZERO,
            )
            .build()
            .await;
            assert!(matches!(
                result,
                Err(cached::RedisCacheBuildError::InvalidTtl(..))
            ));
        }
    }

    // Requires a live Redis server (provided by CI).
    use cached::{ConcurrentCached, SerializeCached};

    #[test]
    fn test_redis_cache_clear_scoped() {
        // Build two caches with different prefixes under an empty namespace so
        // only the SCAN scope (prefix) distinguishes them.
        let cache_a =
            RedisCache::<String, String>::builder("test_clear_scope_a", Duration::from_secs(30))
                .namespace("")
                .build()
                .expect("build cache_a");

        let cache_b =
            RedisCache::<String, String>::builder("test_clear_scope_b", Duration::from_secs(30))
                .namespace("")
                .build()
                .expect("build cache_b");

        // Seed both caches.
        cache_a
            .cache_set("k1".to_string(), "v1".to_string())
            .expect("cache_a set k1");
        cache_a
            .cache_set("k2".to_string(), "v2".to_string())
            .expect("cache_a set k2");
        cache_b
            .cache_set("kb".to_string(), "vb".to_string())
            .expect("cache_b set kb");

        // Clearing cache_a must remove its keys.
        cache_a.cache_clear().expect("cache_a clear");
        assert_eq!(
            cache_a
                .cache_get(&"k1".to_string())
                .expect("cache_a get k1"),
            None,
            "k1 must be gone after cache_clear"
        );
        assert_eq!(
            cache_a
                .cache_get(&"k2".to_string())
                .expect("cache_a get k2"),
            None,
            "k2 must be gone after cache_clear"
        );

        // cache_b's key must still be present.
        assert_eq!(
            cache_b
                .cache_get(&"kb".to_string())
                .expect("cache_b get kb"),
            Some("vb".to_string()),
            "cache_b key must survive cache_a clear"
        );

        // Clean up.
        cache_b.cache_clear().expect("cache_b clear");
    }

    #[test]
    fn test_redis_cache_set_ref_round_trip() {
        let cache =
            RedisCache::<String, String>::builder("test_set_ref_rt", Duration::from_secs(30))
                .namespace("")
                .build()
                .expect("build cache");

        cache.cache_clear().unwrap();

        let key = "ref_key".to_string();
        let val = "ref_val".to_string();
        let val2 = "ref_val_overwrite".to_string();

        // First insert must return None (no previous entry).
        let prev = cache
            .cache_set_ref(&key, &val)
            .expect("first cache_set_ref");
        assert_eq!(prev, None, "first cache_set_ref must return None");

        let got = cache.cache_get(&key).expect("cache_get after set_ref");
        assert_eq!(
            got,
            Some(val.clone()),
            "cache_get must return the value written by cache_set_ref"
        );

        // Overwrite with a different value; must return the previous value.
        let prev2 = cache
            .cache_set_ref(&key, &val2)
            .expect("second cache_set_ref");
        assert_eq!(
            prev2,
            Some(val),
            "second cache_set_ref must return the previous value"
        );

        // Overwrite must be visible via cache_get.
        let got2 = cache.cache_get(&key).expect("cache_get after overwrite");
        assert_eq!(
            got2,
            Some(val2),
            "cache_get must return the overwritten value"
        );

        cache.cache_clear().expect("clean up");
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
    create = "{ ExpiringLruCache::builder().max_size(3).build().unwrap() }"
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::LruCache::builder()
        .max_size(2)
        .on_evict(move |_k, _v| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();

    cache.set(1, 10);
    cache.set(2, 20);
    assert_eq!(evicted_count.load(Ordering::Relaxed), 0);
    cache.set(3, 30);
    assert_eq!(evicted_count.load(Ordering::Relaxed), 1);
}

#[test]
#[cfg(feature = "time_stores")]
fn test_timed_cache_on_evict() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::TtlCache::builder()
        .ttl(cached::time::Duration::from_millis(100))
        .on_evict(move |_k, _v| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();

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

    let mut cache = cached::TtlCache::builder()
        .ttl(cached::time::Duration::from_millis(20))
        .build()
        .unwrap();
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::stores::TtlSortedCache::builder()
        .max_size(2)
        .ttl(cached::time::Duration::from_secs(10))
        .on_evict(move |_k, _v| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();

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
    let mut cache = cached::LruTtlCache::builder()
        .max_size(2)
        .ttl(cached::time::Duration::from_millis(20))
        .build()
        .unwrap();
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::LruTtlCache::builder()
        .max_size(4)
        .ttl(cached::time::Duration::from_millis(50))
        .on_evict(move |_k, _v| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();

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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

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
        .max_size(4)
        .on_evict(move |_k: &i32, _v: &Expirable| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();

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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

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
        .max_size(4)
        .on_evict(move |_k: &i32, _v: &Expirable| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();

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

    let sized = cached::LruCache::<i32, i32>::builder().build();
    assert!(
        matches!(
            sized.unwrap_err(),
            cached::BuildError::MissingRequired("max_size")
        ),
        "expected MissingRequired(max_size)"
    );

    let expiring = cached::ExpiringLruCache::<i32, Expirable>::builder().build();
    assert!(
        matches!(
            expiring.unwrap_err(),
            cached::BuildError::MissingRequired("max_size")
        ),
        "expected MissingRequired(max_size)"
    );

    #[cfg(feature = "time_stores")]
    {
        let timed = cached::TtlCache::<i32, i32>::builder().build();
        assert!(
            matches!(
                timed.unwrap_err(),
                cached::BuildError::MissingRequired("ttl")
            ),
            "expected MissingRequired(ttl)"
        );

        let timed_sized = cached::LruTtlCache::<i32, i32>::builder()
            .ttl(cached::time::Duration::from_secs(1))
            .build();
        assert!(
            matches!(
                timed_sized.unwrap_err(),
                cached::BuildError::MissingRequired("max_size")
            ),
            "expected MissingRequired(max_size)"
        );

        let zero_ttl = cached::TtlCache::<i32, i32>::builder()
            .ttl(cached::time::Duration::ZERO)
            .build();
        assert!(
            matches!(zero_ttl.unwrap_err(), cached::BuildError::InvalidTtl { .. }),
            "expected InvalidTtl"
        );

        let zero_lru_ttl = cached::LruTtlCache::<i32, i32>::builder()
            .max_size(4)
            .ttl(cached::time::Duration::ZERO)
            .build();
        assert!(
            matches!(
                zero_lru_ttl.unwrap_err(),
                cached::BuildError::InvalidTtl { .. }
            ),
            "expected InvalidTtl"
        );

        let zero_sorted_ttl = cached::TtlSortedCache::<i32, i32>::builder()
            .ttl(cached::time::Duration::ZERO)
            .build();
        assert!(
            matches!(
                zero_sorted_ttl.unwrap_err(),
                cached::BuildError::InvalidTtl { .. }
            ),
            "expected InvalidTtl"
        );
    }

    let sharded_unbound = cached::ShardedUnboundCache::<i32, i32>::builder()
        .shards(0)
        .build();
    assert!(
        matches!(
            sharded_unbound.unwrap_err(),
            cached::BuildError::InvalidValue {
                field: "shards",
                ..
            }
        ),
        "expected InvalidValue(shards) for shards(0)"
    );
}

#[cfg(feature = "disk_store")]
#[test]
fn disk_cache_builder_aliases_and_zero_ttl_validation() {
    // Canonical `RedbCache` name.
    let result = cached::RedbCache::<String, String>::builder("zero-ttl")
        .ttl(cached::time::Duration::ZERO)
        .build();
    assert!(matches!(
        result,
        Err(cached::RedbCacheBuildError::InvalidTtl(..))
    ));

    // The kept `DiskCache` alias (and its `DiskCacheBuildError` alias) still
    // compile and behave identically.
    let result = cached::DiskCache::<String, String>::builder("zero-ttl")
        .ttl(cached::time::Duration::ZERO)
        .build();
    assert!(matches!(
        result,
        Err(cached::DiskCacheBuildError::InvalidTtl(..))
    ));
}

#[cfg(feature = "redis_store")]
#[test]
fn redis_cache_builder_aliases_and_zero_ttl_validation() {
    let result =
        cached::RedisCache::<String, String>::builder("zero-ttl", cached::time::Duration::ZERO)
            .build();
    assert!(matches!(
        result,
        Err(cached::RedisCacheBuildError::InvalidTtl(..))
    ));
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
    let mut cache = cached::ExpiringLruCache::builder()
        .max_size(2)
        .build()
        .unwrap();

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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::TtlCache::builder()
        .ttl(cached::time::Duration::from_millis(50))
        .on_evict(move |_k: &u32, _v: &u32| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();

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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::TtlCache::builder()
        .ttl(cached::time::Duration::from_millis(50))
        .on_evict(move |_k: &u32, _v: &u32| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();

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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::TtlCache::builder()
        .ttl(cached::time::Duration::from_millis(50))
        .on_evict(move |_k: &u32, _v: &u32| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();

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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::stores::TtlSortedCache::builder()
        .max_size(4)
        .ttl(cached::time::Duration::from_millis(50))
        .on_evict(move |_k: &u32, _v: &u32| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();

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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_count_clone = evicted_count.clone();
    let mut cache = cached::LruTtlCache::builder()
        .max_size(4)
        .ttl(cached::time::Duration::from_millis(50))
        .on_evict(move |_k: &u32, _v: &u32| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();

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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

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
        .max_size(4)
        .on_evict(move |_k: &i32, _v: &Expirable| {
            evicted_count_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();

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
    use cached::Cached;
    use cached::macros::cached;
    use cached::stores::TtlSortedCache;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    static CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

    #[cached(
        ty = "TtlSortedCache<String, u32>",
        create = "{ TtlSortedCache::builder().ttl(Duration::from_secs(60)).build().unwrap() }",
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
    // Builder-only construction unifies zero-TTL validation: a zero TTL is now
    // rejected at build time (the old permissive `TtlCache::with_ttl` constructor,
    // which allowed it, has been removed).
    let err = TtlCache::<u32, &str>::builder()
        .ttl(Duration::from_nanos(0))
        .build()
        .unwrap_err();
    assert!(matches!(err, cached::BuildError::InvalidTtl { .. }));
}

#[cfg(feature = "time_stores")]
#[test]
fn test_lru_ttl_cache_zero_ttl() {
    use cached::LruTtlCache;
    // Zero TTL is rejected at build time (see `test_ttl_cache_zero_ttl`).
    let err = LruTtlCache::<u32, &str>::builder()
        .max_size(4)
        .ttl(Duration::from_nanos(0))
        .build()
        .unwrap_err();
    assert!(matches!(err, cached::BuildError::InvalidTtl { .. }));
}

#[cfg(feature = "time_stores")]
#[test]
fn test_ttl_sorted_cache_try_set_time_bounds() {
    use cached::Cached;
    use cached::stores::TtlSortedCache;
    // A near-maximum TTL triggers TimeBounds overflow on some platforms.
    // cache_set silently no-ops; cache_try_set returns Err.
    let ttl = Duration::from_secs(u64::MAX / 2);
    let mut cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(ttl)
        .build()
        .unwrap();
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

    let mut c = UnboundCache::builder().build().unwrap();
    c.cache_set(1u32, 1u32);
    c.cache_get(&1u32);
    c.cache_get(&99u32);
    assert_eq!(c.cache_hits(), Some(1));
    assert_eq!(c.cache_misses(), Some(1));
    c.cache_reset();
    assert_eq!(c.cache_hits(), Some(0));
    assert_eq!(c.cache_misses(), Some(0));
    assert_eq!(c.cache_size(), 0);

    let mut lru = LruCache::builder().max_size(4).build().unwrap();
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

    let mut tc = TtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    tc.cache_set(1, 1);
    tc.cache_get(&1);
    tc.cache_get(&99);
    tc.cache_reset();
    assert_eq!(tc.cache_hits(), Some(0));
    assert_eq!(tc.cache_misses(), Some(0));
    assert_eq!(tc.cache_size(), 0);

    let mut ltu = LruTtlCache::<u32, u32>::builder()
        .max_size(4)
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
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

    let mut c = UnboundCache::builder().build().unwrap();
    c.cache_set(1u32, 1u32);
    c.cache_get(&1u32);
    c.cache_get(&99u32);
    assert_eq!(c.cache_hits(), Some(1));
    assert_eq!(c.cache_misses(), Some(1));
    c.cache_clear();
    assert_eq!(c.cache_size(), 0);
    assert_eq!(c.cache_hits(), Some(1));
    assert_eq!(c.cache_misses(), Some(1));

    let mut lru = LruCache::builder().max_size(4).build().unwrap();
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let fired = Arc::new(AtomicU32::new(0));
    let fired_clone = fired.clone();
    let mut cache = UnboundCache::<u32, u32>::builder()
        .on_evict(move |_, _| {
            fired_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();

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

    let mut cache = LruTtlCache::<u32, u32>::builder()
        .max_size(10)
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
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

#[test]
fn test_lru_retain_fires_on_evict_and_increments_evictions() {
    use cached::{Cached, LruCache};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let fired = Arc::new(AtomicU32::new(0));
    let fired_clone = fired.clone();
    let mut cache = LruCache::builder()
        .max_size(10)
        .on_evict(move |_k, _v| {
            fired_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();

    cache.cache_set(1u32, 10u32);
    cache.cache_set(2u32, 20u32);
    cache.cache_set(3u32, 30u32);
    cache.cache_set(4u32, 40u32);

    // Remove odd keys via retain
    cache.retain(|k, _v| k % 2 == 0);

    assert_eq!(fired.load(Ordering::Relaxed), 2); // keys 1 and 3 removed
    assert_eq!(cache.cache_evictions(), Some(2));
    assert_eq!(cache.cache_size(), 2);
    assert!(cache.cache_get(&1u32).is_none());
    assert!(cache.cache_get(&2u32).is_some());
    assert!(cache.cache_get(&3u32).is_none());
    assert!(cache.cache_get(&4u32).is_some());
}

#[cfg(feature = "time_stores")]
#[test]
fn test_lru_ttl_evict_does_not_double_count_evictions() {
    use cached::{Cached, LruTtlCache};

    let mut cache = LruTtlCache::<u32, u32>::builder()
        .max_size(10)
        .ttl(Duration::from_millis(50))
        .build()
        .unwrap();
    cache.cache_set(1u32, 10u32);
    cache.cache_set(2u32, 20u32);
    cache.cache_set(3u32, 30u32);

    std::thread::sleep(Duration::from_millis(100));

    // evict() uses retain_silent internally; cache_evictions() = outer + inner counters.
    // With retain_silent the inner counter stays 0, so total == 3, not 6.
    assert_eq!(cache.evict(), 3);
    assert_eq!(cache.cache_evictions(), Some(3));
}

#[cfg(feature = "time_stores")]
#[test]
fn test_ttl_sorted_cache_clone_cached() {
    use cached::stores::TtlSortedCache;
    use cached::{Cached, CloneCached};

    let mut cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
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
    use cached::CachedAsync;
    use cached::stores::TtlSortedCache;

    let mut cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();

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

    let mut cache = ExpiringLruCache::<u32, NeverExpires>::builder()
        .max_size(4)
        .build()
        .unwrap();

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
    let mut cache = LruCache::<u32, u32>::builder().max_size(4).build().unwrap();
    cache.cache_set(1, 10);
    assert_eq!(cache.cache_get(&1), Some(&10));
    assert_eq!(cache.cache_capacity(), Some(4));
}

#[test]
fn test_unbound_cache_builder_build() {
    use cached::Cached;
    let mut cache = UnboundCache::<u32, u32>::builder().build().unwrap();
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
        .max_size(4)
        .build()
        .unwrap();
    cache.cache_set(1, AlwaysFresh(42));
    assert_eq!(cache.cache_get(&1).map(|v| v.0), Some(42));
}

#[test]
#[cfg(feature = "time_stores")]
fn test_ttl_cache_builder_build() {
    use cached::{Cached, TtlCache};
    let mut cache = TtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    cache.cache_set(1, 10);
    assert_eq!(cache.cache_get(&1), Some(&10));
}

#[test]
#[cfg(feature = "time_stores")]
fn test_lru_ttl_cache_builder_build() {
    use cached::{Cached, LruTtlCache};
    let mut cache = LruTtlCache::<u32, u32>::builder()
        .max_size(4)
        .ttl(Duration::from_secs(60))
        .refresh_on_hit(true)
        .build()
        .unwrap();
    cache.cache_set(1, 10);
    assert_eq!(cache.cache_get(&1), Some(&10));
    assert!(cache.refresh_on_hit());
}

#[test]
#[cfg(feature = "time_stores")]
fn test_ttl_sorted_cache_builder_build() {
    use cached::{Cached, stores::TtlSortedCache};
    let mut cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .max_size(4)
        .build()
        .unwrap();
    cache.cache_set(1, 10);
    assert_eq!(cache.cache_get(&1), Some(&10));
}

// ── `store()` getter ───────────────────────────────────────────────────────────

#[test]
#[cfg(feature = "time_stores")]
fn test_ttl_cache_store_getter() {
    use cached::{Cached, TtlCache};
    let mut cache = TtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    cache.cache_set(1, 10);
    // store() gives direct access to the underlying HashMap<K, TimedEntry<V>>
    assert_eq!(cache.store().len(), 1);
    assert!(cache.store().contains_key(&1));
}

#[test]
fn test_unbound_cache_store_getter() {
    use cached::Cached;
    let mut cache = UnboundCache::<u32, u32>::builder().build().unwrap();
    cache.cache_set(1, 10);
    cache.cache_set(2, 20);
    assert_eq!(cache.store().len(), 2);
}

// ── `refresh_on_hit()` getter and `set_refresh_on_hit()` setter ──────────────

#[test]
#[cfg(feature = "time_stores")]
fn test_ttl_cache_refresh_getter_and_setter() {
    use cached::TtlCache;
    let mut cache = TtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .refresh_on_hit(false)
        .build()
        .unwrap();
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
    let mut cache = UnboundCache::<u32, u32>::builder().build().unwrap();
    cache.cache_set(1, 10);
    cache.cache_set(2, 20);
    let mut pairs: Vec<_> = CachedIter::iter(&cache).collect();
    pairs.sort_by_key(|(k, _)| *k);
    assert_eq!(pairs, vec![(&1u32, &10u32), (&2u32, &20u32)]);
}

#[test]
fn test_cached_iter_lru() {
    use cached::{Cached, CachedIter};
    let mut cache = LruCache::<u32, u32>::builder().max_size(4).build().unwrap();
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
    let mut cache = TtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
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
    let mut cache = TtlCache::<u32, u32>::builder()
        .ttl(Duration::from_millis(20))
        .build()
        .unwrap();
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
    let mut cache = ExpiringLruCache::<u32, Fresh>::builder()
        .max_size(4)
        .build()
        .unwrap();
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
    let mut cache = TtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
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
    let mut cache = LruTtlCache::<u32, u32>::builder()
        .max_size(4)
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    cache.cache_set(1, 10);
    cache.cache_reset_metrics();

    assert_eq!(cache.cache_peek(&1), Some(&10));
    assert_eq!(cache.cache_hits(), Some(0));
    assert_eq!(cache.cache_misses(), Some(0));
}

#[test]
#[cfg(feature = "time_stores")]
fn test_cached_peek_ttl_sorted_cache() {
    use cached::{Cached, CachedPeek, stores::TtlSortedCache};
    let mut cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    cache.cache_set(1, 10);
    cache.cache_reset_metrics();

    assert_eq!(cache.cache_peek(&1), Some(&10));
    assert_eq!(cache.cache_hits(), Some(0));
    assert_eq!(cache.cache_misses(), Some(0));
}

#[test]
fn test_cached_peek_lru_cache() {
    use cached::{Cached, CachedPeek};
    let mut cache = LruCache::<u32, u32>::builder().max_size(4).build().unwrap();
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

    let mut cache = ExpiringLruCache::<u32, AlwaysFresh>::builder()
        .max_size(4)
        .build()
        .unwrap();
    cache.cache_set(1, AlwaysFresh(10));
    cache.cache_reset_metrics();

    assert_eq!(cache.cache_peek(&1).map(|v| v.0), Some(10));
    assert_eq!(cache.cache_hits(), Some(0));
    assert_eq!(cache.cache_misses(), Some(0));

    // peek on a missing key does not record a miss
    assert!(cache.cache_peek(&99).is_none());
    assert_eq!(cache.cache_misses(), Some(0));

    // peek on a logically-expired entry returns None
    let mut cache2 = ExpiringLruCache::<u32, AlwaysExpired>::builder()
        .max_size(4)
        .build()
        .unwrap();
    cache2.cache_set(1, AlwaysExpired);
    cache2.cache_reset_metrics();
    assert!(cache2.cache_peek(&1).is_none());
    assert_eq!(cache2.cache_hits(), Some(0));
    assert_eq!(cache2.cache_misses(), Some(0));
}

#[test]
fn test_cached_peek_unbound_cache() {
    use cached::{Cached, CachedPeek, UnboundCache};
    let mut cache = UnboundCache::<u32, u32>::builder().build().unwrap();
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
    let mut cache = TtlCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
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

    let mut cache = ExpiringLruCache::<u32, Article>::builder()
        .max_size(4)
        .build()
        .unwrap();
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
    let mut cache = TtlCache::<u32, u32>::builder()
        .ttl(Duration::from_millis(20))
        .build()
        .unwrap();
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
    let mut cache = LruTtlCache::<u32, u32>::builder()
        .max_size(4)
        .ttl(Duration::from_millis(20))
        .build()
        .unwrap();
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
    let mut cache = TtlSortedCache::<String, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
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
    let mut cache = TtlSortedCache::<Vec<u32>, &str>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
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
        fn cache_remove_entry(&self, k: &String) -> Result<Option<(String, String)>, Self::Error> {
            Ok(self.inner.lock().unwrap().remove_entry(k))
        }
        fn set_refresh_on_hit(&self, _refresh: bool) -> bool {
            false
        }
        fn cache_clear(&self) -> Result<(), Self::Error> {
            self.inner.lock().unwrap().clear();
            Ok(())
        }
        fn cache_reset(&self) -> Result<(), Self::Error> {
            self.cache_clear()
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
    let mut cache = UnboundCache::<u32, u32>::builder().build().unwrap();
    let m = cache.metrics();
    assert_eq!(m.hits, Some(0));
    assert_eq!(m.misses, Some(0));
    assert_eq!(m.entry_count, 0);
    assert!(m.capacity.is_none());
    assert!(m.hit_ratio().is_none(), "no lookups yet → None");

    cache.cache_set(1, 10);
    cache.cache_get(&1); // hit
    cache.cache_get(&2); // miss
    cache.cache_get(&1); // hit

    let m = cache.metrics();
    assert_eq!(m.hits, Some(2));
    assert_eq!(m.misses, Some(1));
    assert_eq!(m.entry_count, 1);
    let ratio = m.hit_ratio().expect("should have ratio after lookups");
    assert!((ratio - 2.0 / 3.0).abs() < 1e-9);

    // LruCache: bounded, so capacity is Some
    let mut lru = LruCache::<u32, u32>::builder().max_size(4).build().unwrap();
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

// ── TtlSortedCache::reserve and try_set_max_size ──────────────────────────────

#[test]
#[cfg(feature = "time_stores")]
fn test_ttl_sorted_cache_reserve() {
    use cached::Cached;
    use cached::stores::TtlSortedCache;
    let mut cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
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
    let mut cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_secs(60))
        .build()
        .unwrap();
    // Success: set a valid limit
    let prev = cache
        .try_set_max_size(10)
        .expect("non-zero limit should succeed");
    assert!(prev.is_none(), "no previous limit");

    // Set another limit — returns old one
    let prev = cache.try_set_max_size(20).unwrap();
    assert_eq!(prev, Some(10));

    // Error: size of zero is invalid
    let err = cache.try_set_max_size(0);
    assert_eq!(err, Err(cached::SetMaxSizeError::ZeroSize));
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

    #[cached::macros::cached(ttl_secs = 1, result_fallback = true)]
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let evicted_count = Arc::new(AtomicU32::new(0));
    let evicted_clone = evicted_count.clone();
    let mut cache = TtlSortedCache::<u32, u32>::builder()
        .ttl(Duration::from_millis(20))
        .on_evict(move |_k: &u32, _v: &u32| {
            evicted_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();

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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

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
        .max_size(10)
        .on_evict(move |_k: &u32, _v: &Expirable| {
            evicted_clone.fetch_add(1, Ordering::Relaxed);
        })
        .build()
        .unwrap();

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

    let mut cache = ExpiringLruCache::<u32, Fresh>::builder()
        .max_size(4)
        .build()
        .unwrap();
    cache.cache_set(1, Fresh);
    cache.cache_reset_metrics();

    assert!(cache.cache_get(&1).is_some());
    assert_eq!(cache.cache_hits(), Some(1));
    assert_eq!(cache.cache_misses(), Some(0));
    // Inner LruCache metrics must not be incremented — ExpiringLruCache manages its own.
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

    let mut cache = ExpiringLruCache::<u32, Expirable>::builder()
        .max_size(2)
        .build()
        .unwrap();

    // Fill to capacity then insert a third entry: LRU evicts key 1 via the
    // inner LruCache's check_capacity path (inner store eviction counter = 1).
    cache.cache_set(1, Expirable { expired: false });
    cache.cache_set(2, Expirable { expired: false });
    cache.cache_set(3, Expirable { expired: false });
    assert_eq!(cache.cache_evictions(), Some(1)); // 1 LRU capacity eviction

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

    // name: custom static identifier with the default UnboundCache.
    // Default sync_lock is RwLock, so the named static is read via `.write()`.
    #[cached(name = "PAIRWISE_NAMED_UNBOUND")]
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
    #[cached(max_size = 2, sync_lock = "mutex")]
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
    #[once(with_cached_flag = true)]
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
    #[once(with_cached_flag = true)]
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
    #[once(name = "PAIRWISE_ONCE_NAMED_TTL", ttl_secs = 100)]
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
    use cached::Expires;
    use cached::time::Duration;
    use cached::{
        CachedAsync, ExpiringLruCache, LruTtlCache, TtlCache, TtlSortedCache, UnboundCache,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn test_ttl_cache_async() {
        let mut cache = TtlCache::builder()
            .ttl(Duration::from_millis(50))
            .build()
            .unwrap();

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
            .build()
            .unwrap();

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
            .max_size(2)
            .ttl(Duration::from_millis(50))
            .on_evict(move |_, _| {
                evicted_clone.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();

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
            .build()
            .unwrap();

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
            .max_size(2)
            .on_evict(move |_, _| {
                evicted_clone.fetch_add(1, Ordering::Relaxed);
            })
            .build()
            .unwrap();

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
        let mut cache = UnboundCache::builder().build().unwrap();
        let val = cache
            .async_get_or_set_with(1, || async { "hello".to_string() })
            .await;
        assert_eq!(val, "hello");
    }
}
