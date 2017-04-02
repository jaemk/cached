use std::hash::Hash;
use std::cmp::Eq;

pub mod macros;
pub mod caches;

pub use caches::*;


/// Blank `marker` function to help enforce the `cached::Cached` trait on any
/// explicitly specified `Cache` types
pub fn enforce_cached_impl<K: Hash + Eq, V, T: Cached<K, V>>(_: &::std::sync::MutexGuard<T>) {}


pub trait Cached<K, V> {
    fn cache_get(&mut self, k: &K) -> Option<&V>;
    fn cache_set(&mut self, k: K, v: V);
    fn cache_size(&self) -> usize;
    fn cache_hits(&self) -> Option<u32> { None }
    fn cache_misses(&self) -> Option<u32> { None }
    fn cache_capacity(&self) -> Option<u32> { None }
    fn cache_seconds(&self) -> Option<u64> { None }
}


