# cached [![Build Status](https://travis-ci.org/jaemk/cached.svg?branch=master)](https://travis-ci.org/jaemk/cached)

> simple rust caching macro

## Usage

Easy to use caching inspired by python decorators.

```rust
#[macro_use] extern crate cached;
// `cached!` macro requires the `lazy_static!` macro
#[macro_use] extern crate lazy_static;


cached!{ FIB >>
fib(n: u32) -> u32 = {
    if n == 0 || n == 1 { return n; }
    fib(n-1) + fib(n-2)
}}

pub fn main() {
    fib(20);
    fib(20);
    {
        let cache = FIB.lock().unwrap();
        println!("hits: {:?}", cache.cache_hits());
        println!("misses: {:?}", cache.cache_misses());
        // make sure the cache-lock is dropped
    }
    println!("fib(20) = {}", fib(20));
}
```

