/*!
Full tests of macro-defined functions
*/
#[macro_use] extern crate cached;
#[macro_use] extern crate lazy_static;

use std::time::Duration;
use std::thread::sleep;
use cached::{Cached, SizedCache, TimedCache};


cached!{ SIZED_FIB: SizedCache = SizedCache::with_capacity(3); >>
fib1(n: u32) -> u32 = {
    if n == 0 || n == 1 { return n }
    fib1(n-1) + fib1(n-2)
}}


#[test]
fn test_sized_cache() {
    fib1(20);
    {
        let cache = SIZED_FIB.lock().unwrap();
        assert_eq!(3, cache.cache_size());
    }
}


cached!{ TIMED_FIB: TimedCache = TimedCache::with_lifespan_and_capacity(2, 5); >>
fib2(n: u32) -> u32 = {
    if n == 0 || n == 1 { return n }
    fib2(n-1) + fib2(n-2)
}}


#[test]
fn test_timed_cache() {
    fib2(1);
    fib2(1);
    {
        let cache = TIMED_FIB.lock().unwrap();
        assert_eq!(1, cache.cache_misses().unwrap());
        assert_eq!(1, cache.cache_hits().unwrap());
    }
    sleep(Duration::new(2, 0));
    fib2(1);
    {
        let cache = TIMED_FIB.lock().unwrap();
        assert_eq!(2, cache.cache_misses().unwrap());
        assert_eq!(1, cache.cache_hits().unwrap());
    }
}
