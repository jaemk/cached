/*!
Full tests of macro-defined functions
*/
#[macro_use] extern crate cached;
#[macro_use] extern crate lazy_static;

use std::time::Duration;
use std::thread::sleep;
use cached::{UnboundCache, Cached, SizedCache, TimedCache};


cached!{
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


cached!{
    SIZED_FIB: SizedCache<(u32), u32> = SizedCache::with_size(3);
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


cached!{
    TIMED: TimedCache<(u32), u32> = TimedCache::with_lifespan_and_capacity(2, 5);
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
}


cached!{
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


cached_key!{
    TIMED_CACHE: TimedCache<(u32), u32> = TimedCache::with_lifespan_and_capacity(2, 5);
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


cached_key!{
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
    sized_key("a", "b");
    sized_key("a", "b");
    {
        let cache = SIZED_CACHE.lock().unwrap();
        assert_eq!(1, cache.cache_misses().unwrap());
        assert_eq!(1, cache.cache_hits().unwrap());
    }
    sized_key("a", "b");
    {
        let cache = SIZED_CACHE.lock().unwrap();
        assert_eq!(2, cache.cache_hits().unwrap());
    }
}

cached_key_result!{
    RESULT_CACHE_KEY: UnboundCache<(u32), u32> = UnboundCache::new();
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

cached_result!{
    RESULT_CACHE: UnboundCache<(u32), u32> = UnboundCache::new();
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