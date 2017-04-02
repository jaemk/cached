#[macro_use] extern crate cached;
#[macro_use] extern crate lazy_static;

use cached::{Cache, Cached};


cached!{ FIB >>
fib(n: u32) -> u32 = {
    if n == 0 || n == 1 { return n; }
    fib(n-1) + fib(n-2)
}}


cached!{ FIB_CUSTOM: Cache >>
fib_custom(n: u32) -> u32 = {
    if n == 0 || n == 1 { return n; }
    fib_custom(n-1) + fib_custom(n-2)
}}


pub fn main() {
    fib(3);
    fib(3);
    {
        let cache = FIB.lock().unwrap();
        println!("hits: {:?}", cache.hits());
        println!("misses: {:?}", cache.misses());
        // make sure lock is dropped
    }
    fib(10);
    fib(10);

    fib_custom(20);
    fib_custom(20);
    {
        let cache = FIB_CUSTOM.lock().unwrap();
        println!("hits: {:?}", cache.hits());
        println!("misses: {:?}", cache.misses());
        // make sure lock is dropped
    }
    fib_custom(20);
    fib_custom(20);
}
