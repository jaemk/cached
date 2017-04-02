#[macro_use] extern crate cached;
#[macro_use] extern crate lazy_static;

use std::collections::HashMap;
use std::hash::Hash;
use std::cmp::Eq;

use cached::{Cache, Cached};


/// use the default cache
cached!{ FIB >>
fib(n: u32) -> u32 = {
    if n == 0 || n == 1 { return n; }
    fib(n-1) + fib(n-2)
}}


/// use a specific cache
cached!{ FIB_SPECIFIC: Cache >>
fib_specific(n: u32) -> u32 = {
    if n == 0 || n == 1 { return n; }
    fib_specific(n-1) + fib_specific(n-2)
}}


/// implement our own cache
struct MyCache<K: Hash + Eq, V> {
    store: HashMap<K, V>,
}
impl <K: Hash + Eq, V> MyCache<K, V> {
    pub fn new() -> MyCache<K, V> {
        MyCache{store: HashMap::new()}
    }
}
impl <K: Hash + Eq, V> Cached<K, V> for MyCache<K, V> {
    fn cache_get(&mut self, k: &K) -> Option<&V> {
        self.store.get(k)
    }
    fn cache_set(&mut self, k: K, v: V) {
        self.store.insert(k, v);
    }
    fn cache_size(&self) -> usize {
        self.store.len()
    }
}

//cached!{ FIB_CUSTOM: MyCache >>
// ^^ this expects a method `new` on MyCache and will automatically call MyCache::new()
//
// To provide an instantiated cache use:
cached!{ FIB_CUSTOM: MyCache = MyCache::new(); >>
fib_custom(n: u32) -> u32 = {
    if n == 0 || n == 1 { return n; }
    fib_custom(n-1) + fib_custom(n-2)
}}


pub fn main() {
    fib(3);
    fib(3);
    {
        let cache = FIB.lock().unwrap();
        println!("hits: {:?}", cache.cache_hits());
        println!("misses: {:?}", cache.cache_misses());
        // make sure lock is dropped
    }
    fib(10);
    fib(10);

    fib_specific(20);
    fib_specific(20);
    {
        let cache = FIB_SPECIFIC.lock().unwrap();
        println!("hits: {:?}", cache.cache_hits());
        println!("misses: {:?}", cache.cache_misses());
        // make sure lock is dropped
    }
    fib_specific(20);
    fib_specific(20);

    fib_custom(25);
    {
        let cache = FIB_CUSTOM.lock().unwrap();
        println!("hits: {:?}", cache.cache_hits());
        println!("misses: {:?}", cache.cache_misses());
        // make sure lock is dropped
    }
}
