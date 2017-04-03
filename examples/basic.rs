#[macro_use] extern crate cached;
#[macro_use] extern crate lazy_static;

use std::collections::HashMap;
use std::hash::Hash;
use std::cmp::Eq;

use std::time::Duration;
use std::thread::sleep;

use cached::{UnboundCache, Cached};


/// Use the default unbounded cache, `Cache`.
/// Equivalent to specifying `FIB: Cache`
cached!{ FIB >>
fib(n: u32) -> u32 = {
    if n == 0 || n == 1 { return n; }
    fib(n-1) + fib(n-2)
}}


/// Specify the cache type.
cached!{ FIB_SPECIFIC: UnboundCache >>
fib_specific(n: u32) -> u32 = {
    if n == 0 || n == 1 { return n; }
    fib_specific(n-1) + fib_specific(n-2)
}}


/// Implement our own cache type
struct MyCache<K: Hash + Eq, V> {
    store: HashMap<K, V>,
}
impl <K: Hash + Eq, V> MyCache<K, V> {
    pub fn with_capacity(size: usize) -> MyCache<K, V> {
        MyCache { store: HashMap::with_capacity(size) }
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


//cached!{ CUSTOM: MyCache >>
// ^^ this expects a method `new` on MyCache and will automatically call MyCache::new()
//
// To provide an instantiated cache use the following:
/// Specify our custom cache and supply an instance to use
cached!{ CUSTOM: MyCache = MyCache::with_capacity(50); >>
custom(n: u32) -> () = {
    if n == 0 { return; }
    custom(n-1)
}}



cached!{ SLOW >>
slow(n: u32) -> u32 = {
    if n == 0 || n == 1 { return n }
    sleep(Duration::new(1, 0));
    slow(n-1) + slow(n-2)
}}


pub fn main() {
    println!("\n ** default cache **");
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

    println!("\n ** specific cache **");
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

    println!("\n ** custom cache **");
    custom(25);
    {
        let cache = CUSTOM.lock().unwrap();
        println!("hits: {:?}", cache.cache_hits());
        println!("misses: {:?}", cache.cache_misses());
        // make sure lock is dropped
    }

    println!("\n ** slow func **");
    println!(" - first run `slow(10)`");
    slow(10);
    println!(" - second run `slow(10)`");
    slow(10);
    {
        let cache = SLOW.lock().unwrap();
        println!("hits: {:?}", cache.cache_hits());
        println!("misses: {:?}", cache.cache_misses());
        // make sure the cache-lock is dropped
    }
}
