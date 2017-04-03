/*!
A macro for defining functions that wrap a static-ref cache object.

# Options:

1.) Use the default unbounded cache


```rust,ignore
cached!{CACHE_NAME >>
func_name(arg1: arg1_type, arg2: arg2_type) -> return_type = {
    <regular function body>
}}
```


2.) Use an explicitly specified cache-type, but let the macro instantiate it.
    The cache-type is expected to have a `new` method that takes no arguments.


```rust,ignore
cached!{CACHE_NAME: SpecificCacheType >>
func_name(arg1: arg1_type, arg2: arg2_type) -> return_type = {
    <regular function body>
}}
```


3.) Use an explicitly specified cache-type and provide the instantiated cache struct.
    This allows using caches that require args in their constructor or have a constructor
    method other than a simple `new`.


```rust,ignore
cached!{CACHE_NAME: MyCache = MyCache::with_capacity(arg); >>
func_name(arg1: arg1_type, arg2: arg2_type) -> return_type = {
    <regular function body>
}}
```


Custom cache types must implement `cached::Cached`

*/

use std::hash::Hash;
use std::cmp::Eq;

pub mod macros;
pub mod stores;

pub use stores::*;


/// Blank marker function to help enforce the `cached::Cached` trait on any
/// explicitly specified cache types
pub fn enforce_cached_impl<K: Hash + Eq, V, T: Cached<K, V>>(_: &::std::sync::MutexGuard<T>) {}


pub trait Cached<K, V> {
    fn cache_get(&mut self, k: &K) -> Option<&V>;
    fn cache_set(&mut self, k: K, v: V);
    fn cache_size(&self) -> usize;
    fn cache_hits(&self) -> Option<u32> { None }
    fn cache_misses(&self) -> Option<u32> { None }
    fn cache_capacity(&self) -> Option<usize> { None }
    fn cache_lifespan(&self) -> Option<u64> { None }
}


