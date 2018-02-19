/*!
Full tests of macro-defined functions
*/
#[macro_use] extern crate cached;
#[macro_use] extern crate lazy_static;

use std::time::Duration;
use std::thread::sleep;
use cached::{Cached, SizedCache, TimedCache};


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
    SIZED_FIB: SizedCache<(u32), u32> = SizedCache::with_capacity(3);
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
    STRING_CACHE_EXPLICIT: SizedCache<(String, String), String> = SizedCache::with_capacity(1);
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

