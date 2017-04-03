/*!
Full tests of macro-defined functions
*/
#[macro_use] extern crate cached;
#[macro_use] extern crate lazy_static;

use cached::{Cached, SizedCache};


cached!{ SIZED_FIB: SizedCache = SizedCache::with_capacity(3); >>
fib1(n: u32) -> u32 = {
    if n == 0 || n == 1 { return n }
    fib1(n-1) + fib1(n-2)
}}


#[test]
fn test_basic_cache() {
    fib1(20);
    {
        let cache = SIZED_FIB.lock().unwrap();
        assert_eq!(3, cache.cache_size());
    }
}
