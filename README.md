# cached [![Build Status](https://travis-ci.org/jaemk/cached.svg?branch=master)](https://travis-ci.org/jaemk/cached) [![crates.io](https://img.shields.io/crates/v/cached.svg)](https://crates.io/crates/cached) [![docs](https://docs.rs/cached/badge.svg)](https://docs.rs/cached)

> simple rust caching macro

Easy to use function caching/memoization inspired by python decorators.

Function results are cached using the function's arguments as a key.
When a `cached!` defined function is called, the function's cache is first checked for an already
computed (and still valid) value before evaluating the function body.
Due to the requirements of storing arguments and return values in a global cache,
function arguments and return types must be owned and the function's signature must
follow `fn<T: Hash + Eq + Clone, U: Clone>(arg: T) -> U`.
Arguments and return values will be `cloned` in the process of insertion and retrieval.
`cached!` functions should not be used to produce side-effectual results!

[Documentation](https://docs.rs/cached)

See `examples` for example of implementing a custom cache-store.

## Usage


```rust
#[macro_use] extern crate cached;
// `cached!` macro requires the `lazy_static!` macro
#[macro_use] extern crate lazy_static;

use std::time::{Instant, Duration};
use std::thread::sleep;

use cached::SizedCache;


cached!{ SLOW: SizedCache = SizedCache::with_capacity(50); >>
fn slow(n: u32) -> String = {
    if n == 0 { return "done".to_string(); }
    sleep(Duration::new(1, 0));
    slow(n-1)
}}

pub fn main() {
    println!("running fresh...");
    let now = Instant::now();
    let _ = slow(10);
    println!("fresh! elapsed: {}", now.elapsed().as_secs());

    println!("running cached...");
    let now = Instant::now();
    let _ = slow(10);
    println!("cached!! elapsed: {}", now.elapsed().as_secs());

    {
        use cached::Cached;  // must be in scope to access cache

        println!(" ** Cache info **");
        let cache = SLOW.lock().unwrap();
        println!("hits=1 -> {:?}", cache.cache_hits().unwrap() == 1);
        println!("misses=11 -> {:?}", cache.cache_misses().unwrap() == 11);
        // make sure the cache-lock is dropped
    }
}
```

