# cached

[![Build Status](https://travis-ci.org/jaemk/cached.svg?branch=master)](https://travis-ci.org/jaemk/cached)
[![crates.io](https://img.shields.io/crates/v/cached.svg)](https://crates.io/crates/cached)
[![docs](https://docs.rs/cached/badge.svg)](https://docs.rs/cached)

> Caching structures and simplified function memoization

`cached` provides implementations of several caching structures as well as a handy macro
for defining memoized functions.


## Defining memoized functions using `cached!`

`cached!` defined functions will have their results cached using the function's arguments as a key
(or a specific expression when using `cached_key!`).
When a `cached!` defined function is called, the function's cache is first checked for an already
computed (and still valid) value before evaluating the function body.

Due to the requirements of storing arguments and return values in a global cache:

- Function return types must be owned and implement `Clone`
- Function arguments must either be owned and implement `Hash + Eq + Clone` OR the `cached_key!`
  macro must be used to convert arguments into an owned + `Hash + Eq + Clone` type.
- Arguments and return values will be `cloned` in the process of insertion and retrieval.
- `cached!` functions should not be used to produce side-effectual results!

**NOTE**: Any custom cache that implements `cached::Cached` can be used with the `cached` macros in place of the built-ins.

See [`examples`](https://github.com/jaemk/cached/tree/master/examples) for basic usage and
an example of implementing a custom cache-store.


### `cached!` and `cached_key!` Usage & Options:

There are several options depending on how explicit you want to be. See below for a full syntax breakdown.


1.) Using the shorthand will use an unbounded cache.


```rust
#[macro_use] extern crate cached;
#[macro_use] extern crate lazy_static;

/// Defines a function named `fib` that uses a cache named `FIB`
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
    of the function argument types. If you would like fine grained control over
    the key, you can use the `cached_key!` macro.
    The follow example uses a `SizedCache` (LRU):

```rust
#[macro_use] extern crate cached;
#[macro_use] extern crate lazy_static;

use std::thread::sleep;
use std::time::Duration;
use cached::SizedCache;

/// Defines a function `fib` that uses an LRU cache named `FIB` which has a
/// size limit of 50 items. The `cached!` macro will implicitly combine
/// the function arguments into a tuple to be used as the cache key.
cached!{
    FIB: SizedCache<(u64, u64), u64> = SizedCache::with_size(50);
    fn fib(a: u64, b: u64) -> u64 = {
        sleep(Duration::new(2, 0));
        return a * b;
    }
}
```


3.) The `cached_key` macro functions identically, but allows you define the
    cache key as an expression.

```rust
#[macro_use] extern crate cached;
#[macro_use] extern crate lazy_static;

use std::thread::sleep;
use std::time::Duration;
use cached::SizedCache;

/// Defines a function named `fib` that uses an LRU cache named `FIB`.
/// The `Key = ` expression is used to explicitly define the value that
/// should be used as the cache key. Here the borrowed arguments are converted
/// to an owned string that can be stored in the global function cache.
cached_key!{
    FIB: SizedCache<String, usize> = SizedCache::with_size(50);
    Key = { format!("{}{}", a, b) };
    fn fib(a: &str, b: &str) -> usize = {
        let size = a.len() + b.len();
        sleep(Duration::new(size as u64, 0));
        size
    }
}
```


## Syntax

The complete macro syntax is:


```rust
cached_key!{
    CACHE_NAME: CacheType = CacheInstance;
    Key = KeyExpression;
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
- When using the `cached_key!` macro, the "Key" line must be specified. This line must start with
  the literal tokens `Key = `, followed by an expression that evaluates to the key, followed by `;`
- `fn func_name(arg1: arg_type) -> return_type` is the same form as a regular function signature, with the exception
  that functions with no return value must be explicitly stated (e.g. `fn func_name(arg: arg_type) -> ()`)
- The expression following `=` is the function body assigned to `func_name`. Note, the function
  body can make recursive calls to its cached-self (`func_name`).


License: MIT
