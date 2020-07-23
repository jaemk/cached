/*!
Full tests of macro-defined functions
*/
#[macro_use]
extern crate cached;

use cached::{proc_macro::cached, Cached, SizedCache, TimedCache, UnboundCache};
use std::thread::{self, sleep};
use std::time::Duration;

cached! {
    UNBOUND_FIB;
    fn fib0(n: u32) -> u32 = {
        if n == 0 || n == 1 { return n }
        fib0(n-1) + fib0(n-2)
    }
}

#[test]
fn test_unbound_cache() {
    fib0(20);
    {
        let cache = UNBOUND_FIB.lock().unwrap();
        assert_eq!(21, cache.cache_size());
    }
}

cached! {
    SIZED_FIB: SizedCache<u32, u32> = SizedCache::with_size(3);
    fn fib1(n: u32) -> u32 = {
        if n == 0 || n == 1 { return n }
        fib1(n-1) + fib1(n-2)
    }
}

#[test]
fn test_sized_cache() {
    fib1(20);
    {
        let cache = SIZED_FIB.lock().unwrap();
        assert_eq!(3, cache.cache_size());
    }
}

cached! {
    TIMED: TimedCache<u32, u32> = TimedCache::with_lifespan_and_capacity(2, 5);
    fn timed(n: u32) -> u32 = {
        sleep(Duration::new(3, 0));
        n
    }
}

#[test]
fn test_timed_cache() {
    timed(1);
    timed(1);
    {
        let cache = TIMED.lock().unwrap();
        assert_eq!(1, cache.cache_misses().unwrap());
        assert_eq!(1, cache.cache_hits().unwrap());
    }
    sleep(Duration::new(3, 0));
    timed(1);
    {
        let cache = TIMED.lock().unwrap();
        assert_eq!(2, cache.cache_misses().unwrap());
        assert_eq!(1, cache.cache_hits().unwrap());
    }
    {
        let mut cache = TIMED.lock().unwrap();
        assert_eq!(2, cache.cache_set_lifespan(1).unwrap());
    }
    timed(1);
    sleep(Duration::new(1, 0));
    timed(1);
    {
        let cache = TIMED.lock().unwrap();
        assert_eq!(3, cache.cache_misses().unwrap());
        assert_eq!(2, cache.cache_hits().unwrap());
    }
}

cached! {
    STRING_CACHE_EXPLICIT: SizedCache<(String, String), String> = SizedCache::with_size(1);
    fn string_1(a: String, b: String) -> String = {
        return a + &b;
    }
}

#[test]
fn test_string_cache() {
    string_1("a".into(), "b".into());
    {
        let cache = STRING_CACHE_EXPLICIT.lock().unwrap();
        assert_eq!(1, cache.cache_size());
    }
}

cached_key! {
    TIMED_CACHE: TimedCache<u32, u32> = TimedCache::with_lifespan_and_capacity(2, 5);
    Key = { n };
    fn timed_2(n: u32) -> u32 = {
        sleep(Duration::new(3, 0));
        n
    }
}

#[test]
fn test_timed_cache_key() {
    timed_2(1);
    timed_2(1);
    {
        let cache = TIMED_CACHE.lock().unwrap();
        assert_eq!(1, cache.cache_misses().unwrap());
        assert_eq!(1, cache.cache_hits().unwrap());
    }
    sleep(Duration::new(3, 0));
    timed_2(1);
    {
        let cache = TIMED_CACHE.lock().unwrap();
        assert_eq!(2, cache.cache_misses().unwrap());
        assert_eq!(1, cache.cache_hits().unwrap());
    }
}

cached_key! {
    SIZED_CACHE: SizedCache<String, usize> = SizedCache::with_size(2);
    Key = { format!("{}{}", a, b) };
    fn sized_key(a: &str, b: &str) -> usize = {
        let size = a.len() + b.len();
        sleep(Duration::new(size as u64, 0));
        size
    }
}

#[test]
fn test_sized_cache_key() {
    sized_key("a", "1");
    sized_key("a", "1");
    {
        let cache = SIZED_CACHE.lock().unwrap();
        assert_eq!(1, cache.cache_misses().unwrap());
        assert_eq!(1, cache.cache_hits().unwrap());
        assert_eq!(1, cache.cache_size());
    }
    sized_key("a", "1");
    {
        let cache = SIZED_CACHE.lock().unwrap();
        assert_eq!(1, cache.cache_misses().unwrap());
        assert_eq!(2, cache.cache_hits().unwrap());
        assert_eq!(1, cache.cache_size());
    }
    sized_key("a", "2");
    {
        let cache = SIZED_CACHE.lock().unwrap();
        assert_eq!(2, cache.cache_hits().unwrap());
        assert_eq!(2, cache.cache_size());
        assert_eq!(vec!["a2", "a1"], cache.key_order().collect::<Vec<_>>());
        assert_eq!(vec![&2, &2], cache.value_order().collect::<Vec<_>>());
    }
    sized_key("a", "3");
    {
        let cache = SIZED_CACHE.lock().unwrap();
        assert_eq!(2, cache.cache_size());
        assert_eq!(vec!["a3", "a2"], cache.key_order().collect::<Vec<_>>());
        assert_eq!(vec![&2, &2], cache.value_order().collect::<Vec<_>>());
    }
    sized_key("a", "4");
    sized_key("a", "5");
    {
        let cache = SIZED_CACHE.lock().unwrap();
        assert_eq!(2, cache.cache_size());
        assert_eq!(vec!["a5", "a4"], cache.key_order().collect::<Vec<_>>());
        assert_eq!(vec![&2, &2], cache.value_order().collect::<Vec<_>>());
    }
    sized_key("a", "67");
    sized_key("a", "8");
    {
        let cache = SIZED_CACHE.lock().unwrap();
        assert_eq!(2, cache.cache_size());
        assert_eq!(vec!["a8", "a67"], cache.key_order().collect::<Vec<_>>());
        assert_eq!(vec![&2, &3], cache.value_order().collect::<Vec<_>>());
    }
}

cached_key_result! {
    RESULT_CACHE_KEY: UnboundCache<u32, u32> = UnboundCache::new();
    Key = { n };
    fn test_result_key(n: u32) -> Result<u32, ()> = {
        if n < 5 { Ok(n) } else { Err(()) }
    }
}

#[test]
fn cache_result_key() {
    assert!(test_result_key(2).is_ok());
    assert!(test_result_key(4).is_ok());
    assert!(test_result_key(6).is_err());
    assert!(test_result_key(6).is_err());
    assert!(test_result_key(2).is_ok());
    assert!(test_result_key(4).is_ok());
    {
        let cache = RESULT_CACHE_KEY.lock().unwrap();
        assert_eq!(2, cache.cache_size());
        assert_eq!(2, cache.cache_hits().unwrap());
        assert_eq!(4, cache.cache_misses().unwrap());
    }
}

cached_result! {
    RESULT_CACHE: UnboundCache<u32, u32> = UnboundCache::new();
    fn test_result_no_default(n: u32) -> Result<u32, ()> = {
        if n < 5 { Ok(n) } else { Err(()) }
    }
}

#[test]
fn cache_result_no_default() {
    assert!(test_result_no_default(2).is_ok());
    assert!(test_result_no_default(4).is_ok());
    assert!(test_result_no_default(6).is_err());
    assert!(test_result_no_default(6).is_err());
    assert!(test_result_no_default(2).is_ok());
    assert!(test_result_no_default(4).is_ok());
    {
        let cache = RESULT_CACHE.lock().unwrap();
        assert_eq!(2, cache.cache_size());
        assert_eq!(2, cache.cache_hits().unwrap());
        assert_eq!(4, cache.cache_misses().unwrap());
    }
}

cached_control! {
    CONTROL_CACHE: UnboundCache<String, String> = UnboundCache::new();
    Key = { input.to_owned() };
    PostGet(cached_val) = return Ok(cached_val.clone());
    PostExec(body_result) = {
        match body_result {
            Ok(v) => v,
            Err(e) => return Err(e),
        }
    };
    Set(set_value) = set_value.clone();
    Return(return_value) = {
        println!("{}", return_value);
        Ok(return_value)
    };
    fn can_fail(input: &str) -> Result<String, String> = {
        let len = input.len();
        if len < 3 { Ok(format!("{}-{}", input, len)) }
        else { Err("too big".to_string()) }
    }
}

#[test]
fn test_can_fail() {
    assert_eq!(can_fail("ab"), Ok("ab-2".to_string()));
    assert_eq!(can_fail("abc"), Err("too big".to_string()));
    {
        let cache = CONTROL_CACHE.lock().unwrap();
        assert_eq!(2, cache.cache_misses().unwrap());
    }
    assert_eq!(can_fail("ab"), Ok("ab-2".to_string()));
    {
        let cache = CONTROL_CACHE.lock().unwrap();
        assert_eq!(1, cache.cache_hits().unwrap());
    }
}

cached_key! {
    SIZED_KEY_RESULT_CACHE: SizedCache<String, String> = SizedCache::with_size(2);
    Key = { format!("{}/{}", a, b) };
    fn slow_small_cache(a: &str, b: &str) -> String = {
        sleep(Duration::new(1, 0));
        format!("{}:{}", a, b)
    }
}

#[test]
/// This is a regression test to confirm that racing cache sets on a SizedCache
/// do not cause duplicates to exist in the internal `order`. See issue #7
fn test_racing_duplicate_keys_do_not_duplicate_sized_cache_ordering() {
    let a = thread::spawn(|| slow_small_cache("a", "b"));
    sleep(Duration::new(0, 500000));
    let b = thread::spawn(|| slow_small_cache("a", "b"));
    a.join().unwrap();
    b.join().unwrap();
    // at this point, the cache should have a size of one since the keys are the same
    // and the internal `order` list should also have one item.
    // Since the method's cache has a capacity of 2, caching two more unique keys should
    // force the full eviction of the original values.
    slow_small_cache("c", "d");
    slow_small_cache("e", "f");
    slow_small_cache("g", "h");
}

// NoClone is not cloneable. So this also tests that the Result type
// itself does not have to be cloneable, just the type for the Ok
// value.
// Vec has Clone, but not Copy, to make sure Copy isn't required.
struct NoClone {}

#[cached(result = true)]
fn proc_cached_result(n: u32) -> Result<Vec<u32>, NoClone> {
    if n < 5 {
        Ok(vec![n])
    } else {
        Err(NoClone {})
    }
}

#[test]
fn test_proc_cached_result() {
    assert!(proc_cached_result(2).is_ok());
    assert!(proc_cached_result(4).is_ok());
    assert!(proc_cached_result(6).is_err());
    assert!(proc_cached_result(6).is_err());
    assert!(proc_cached_result(2).is_ok());
    assert!(proc_cached_result(4).is_ok());
    {
        let cache = PROC_CACHED_RESULT.lock().unwrap();
        assert_eq!(2, cache.cache_size());
        assert_eq!(2, cache.cache_hits().unwrap());
        assert_eq!(4, cache.cache_misses().unwrap());
    }
}

#[cached(option = true)]
fn proc_cached_option(n: u32) -> Option<Vec<u32>> {
    if n < 5 {
        Some(vec![n])
    } else {
        None
    }
}

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
        let cache = PROC_CACHED_OPTION.lock().unwrap();
        assert_eq!(3, cache.cache_size());
        assert_eq!(3, cache.cache_hits().unwrap());
        assert_eq!(5, cache.cache_misses().unwrap());
    }
}
