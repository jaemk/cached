/*!
A macro for defining cached/memoized functions that wrap a static-ref cache object.

# Usage & Options:

There's several option depending on how explicit you want to be. See below for full syntax breakdown.

1.) Use an explicitly specified cache-type and provide the instantiated cache struct.
    For example, a `SizedCache` (LRU).


```rust
#[macro_use] extern crate cached;
#[macro_use] extern crate lazy_static;

use cached::SizedCache;

cached!{FIB: SizedCache = SizedCache::with_capacity(50); >>
fn fib(n: u64) -> u64 = {
    if n == 0 || n == 1 { return n }
    fib(n-1) + fib(n-2)
}}

pub fn main() { }
```


2.) Use an explicitly specified cache-type, but let the macro instantiate it.
    The cache-type is expected to have a `new` method that takes no arguments.


```rust
#[macro_use] extern crate cached;
#[macro_use] extern crate lazy_static;

use cached::UnboundCache;

cached!{FIB: UnboundCache >>
fn fib(n: u64) -> u64 = {
    if n == 0 || n == 1 { return n }
    fib(n-1) + fib(n-2)
}}

pub fn main() { }
```


3.) Use the default unbounded cache.


```rust
#[macro_use] extern crate cached;
#[macro_use] extern crate lazy_static;

cached!{FIB >>
fn fib(n: u64) -> u64 = {
    if n == 0 || n == 1 { return n }
    fib(n-1) + fib(n-2)
}}

pub fn main() { }
```


# Cache Types

Several caches are available in this crate:

- `cached::UnboundCache`
- `cached::SizedCache`
- `cached::TimedCache`

Any custom cache that implements `cached::Cached` can be used in place of the built-ins.

# Syntax:

The complete macro syntax is:


```rust,ignore
cached!{CACHE_NAME: CacheType = CacheType::constructor(arg); >>
fn func_name(arg1: arg_type, arg2: arg_type) -> return_type = {
    // do stuff like normal
    return_type
}}
```

Where:

- `CACHE_NAME` is the unique name used to hold a `static ref` to the cache
- `CacheType` is the struct type to use for the cache (Note, this cannot be namespaced, e.g.
  `cached::SizedCache` will not be accepted by the macro. `SizedCache` must be imported and passed
   directly)
- `CacheType::constructor(arg)` is any expression that yields an instance of `CacheType` to be used
  as the cache-store, followed by `; >>`
- `fn func_name(arg1: arg_type) -> return_type` is the same form as a regular function signature, with the exception
  that functions with no return value must be explicitly stated (e.g. `fn func_name(arg: arg_type) -> ()`)
- The expression following `=` is the function body assigned to `func_name`. Note, the function
  body can make recursive calls to its cached-self (`func_name`).


*/

pub mod macros;
pub mod stores;

pub use stores::*;


pub trait Cached<K, V> {
    fn cache_get(&mut self, k: &K) -> Option<&V>;
    fn cache_set(&mut self, k: K, v: V);
    fn cache_size(&self) -> usize;
    fn cache_hits(&self) -> Option<u32> { None }
    fn cache_misses(&self) -> Option<u32> { None }
    fn cache_capacity(&self) -> Option<usize> { None }
    fn cache_lifespan(&self) -> Option<u64> { None }
}


