# cached

[![Build Status](https://travis-ci.org/jaemk/cached.svg?branch=master)](https://travis-ci.org/jaemk/cached)
[![crates.io](https://img.shields.io/crates/v/cached.svg)](https://crates.io/crates/cached)
[![docs](https://docs.rs/cached/badge.svg)](https://docs.rs/cached)

> Caching structures and simplified function memoization

`cached` provides implementations of several caching structures as well as a handy macro
for defining memoized functions.


## Defining memoized functions using `cached!`

`cached!` defined functions will have their results cached using the function's arguments as a key.
When a `cached!` defined function is called, the function's cache is first checked for an already
computed (and still valid) value before evaluating the function body.

Due to the requirements of storing arguments and return values in a global cache,
function arguments and return types must be owned, function arguments must implement `Hash + Eq + Clone`,
and function return types must implement `Clone`.
Arguments and return values will be `cloned` in the process of insertion and retrieval.
`cached!` functions should not be used to produce side-effectual results!

**NOTE**: Any custom cache that implements `cached::Cached` can be used with the `cached!` macro in place of the built-ins.

See [`examples`](https://github.com/jaemk/cached/tree/master/examples) for basic usage and
an example of implementing a custom cache-store.


### `cached!` Usage & Options:

There are two options depending on how explicit you want to be. See below for a full syntax breakdown.


1.) Using the shorthand will use an unbounded cache.


```rust
#[macro_use] extern crate cached;
#[macro_use] extern crate lazy_static;

cached!{
    FIB;
    fn fib(n: u64) -> u64 = {
        if n == 0 || n == 1 { return n }
        fib(n-1) + fib(n-2)
    }
}
```


2.) Using the full syntax requires specifying the full cache type and providing
    an instance of the cache to use. Note that the cache's key-type is a tuple
    of the function argument types. For example, a `SizedCache` (LRU):

```rust
#[macro_use] extern crate cached;
#[macro_use] extern crate lazy_static;

use std::thread::sleep;
use std::time::Duration;
use cached::SizedCache;

cached!{
    FIB: SizedCache<(u64, u64), u64> = SizedCache::with_capacity(50);
    fn fib(a: u64, b: u64) -> u64 = {
        sleep(Duration::new(2, 0));
        return a * b;
    }
}
```


## Syntax

The complete macro syntax is:


```rust
cached!{
    CACHE_NAME: CacheType = CacheInstance;
    fn func_name(arg1: arg_type, arg2: arg_type) -> return_type = {
        // do stuff like normal
        return_type
    }
}
```

Where:

- `CACHE_NAME` is the unique name used to hold a `static ref` to the cache
- `CacheType` is the full type of the cache
- `CacheInstance` is any expression that yields an instance of `CacheType` to be used
  as the cache-store, followed by `;`
- `fn func_name(arg1: arg_type) -> return_type` is the same form as a regular function signature, with the exception
  that functions with no return value must be explicitly stated (e.g. `fn func_name(arg: arg_type) -> ()`)
- The expression following `=` is the function body assigned to `func_name`. Note, the function
  body can make recursive calls to its cached-self (`func_name`).


License: MIT
