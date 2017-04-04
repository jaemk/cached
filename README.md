# cached [![Build Status](https://travis-ci.org/jaemk/cached.svg?branch=master)](https://travis-ci.org/jaemk/cached) [![crates.io](https://img.shields.io/crates/v/cached.svg)](https://crates.io/crates/cached) [![docs](https://docs.rs/cached/badge.svg)](https://docs.rs/cached)

> simple rust caching macro

Easy to use caching inspired by python decorators.

[Documentation](https://docs.rs/cached)

See `examples` for example of implementing a custom cache-store.

## Usage


```rust
#[macro_use] extern crate cached;
// `cached!` macro requires the `lazy_static!` macro
#[macro_use] extern crate lazy_static;

use std::time::Duration;
use std::thread::sleep;

use cached::SizedCache;


cached!{ SLOW: SizedCache = SizedCache::with_capacity(50); >>
slow(n: u32) -> () = {
    if n == 0 { return; }
    sleep(Duration::new(1, 0));
    slow(n-1)
}}

pub fn main() {
    slow(10);
    slow(10);
    {
        let cache = SLOW.lock().unwrap();
        println!("hits: {:?}", cache.cache_hits());
        println!("misses: {:?}", cache.cache_misses());
        // make sure the cache-lock is dropped
    }
}
```

