use std::cmp::Eq;
use std::collections::HashMap;
use std::hash::Hash;

use std::thread::sleep;
use std::time::Duration;

use cached::proc_macro::cached;
use cached::{Cached, SizedCache, UnboundCache};

// cached shorthand, uses the default unbounded cache.
// Equivalent to specifying `type = "UnboundCache<(u32), u32>", create= "{ UnboundCache::new() }"`
#[cached]
fn fib(n: u32) -> u32 {
    if n == 0 || n == 1 {
        return n;
    }
    fib(n - 1) + fib(n - 2)
}

#[cached(name = "FLIB")]
fn fib_2(n: u32) -> u32 {
    if n == 0 || n == 1 {
        return n;
    }
    fib(n - 1) + fib(n - 2)
}

// Same as above, but preallocates some space.
#[cached(
    type = "UnboundCache<u32, u32>",
    create = "{ UnboundCache::with_capacity(50) }"
)]
fn fib_specific(n: u32) -> u32 {
    if n == 0 || n == 1 {
        return n;
    }
    fib_specific(n - 1) + fib_specific(n - 2)
}

// Specify a specific cache type
// Note that the cache key type is a tuple of function argument types.
#[cached(
    type = "SizedCache<(u32, u32), u32>",
    create = "{ SizedCache::with_size(100) }"
)]
fn slow(a: u32, b: u32) -> u32 {
    sleep(Duration::new(2, 0));
    a * b
}

// Specify a specific cache type and an explicit key expression
// Note that the cache key type is a `String` created from the borrow arguments
// Note that key is not used, convert requires either key or type to be set.
#[cached(
    type = "SizedCache<String, usize>",
    create = "{ SizedCache::with_size(100) }",
    convert = r#"{ format!("{}{}", a, b) }"#
)]
fn keyed(a: &str, b: &str) -> usize {
    let size = a.len() + b.len();
    sleep(Duration::new(size as u64, 0));
    size
}

// Implement our own cache type
struct MyCache<K: Hash + Eq, V> {
    store: HashMap<K, V>,
    capacity: usize,
}
impl<K: Hash + Eq, V> MyCache<K, V> {
    pub fn with_capacity(size: usize) -> MyCache<K, V> {
        MyCache {
            store: HashMap::with_capacity(size),
            capacity: size,
        }
    }
}
impl<K: Hash + Eq, V> Cached<K, V> for MyCache<K, V> {
    fn cache_get(&mut self, k: &K) -> Option<&V> {
        self.store.get(k)
    }
    fn cache_get_mut(&mut self, k: &K) -> Option<&mut V> {
        self.store.get_mut(k)
    }
    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, k: K, f: F) -> &mut V {
        self.store.entry(k).or_insert_with(f)
    }
    fn cache_set(&mut self, k: K, v: V) -> Option<V> {
        self.store.insert(k, v)
    }
    fn cache_remove(&mut self, k: &K) -> Option<V> {
        self.store.remove(k)
    }
    fn cache_clear(&mut self) {
        self.store.clear();
    }
    fn cache_reset(&mut self) {
        self.store = HashMap::with_capacity(self.capacity);
    }
    fn cache_size(&self) -> usize {
        self.store.len()
    }
}

// Specify our custom cache and supply an instance to use
#[cached(type = "MyCache<u32, ()>", create = "{ MyCache::with_capacity(50) }")]
fn custom(n: u32) -> () {
    if n == 0 {
        return;
    }
    custom(n - 1)
}

#[cached(result = true)]
fn slow_result(a: u32, b: u32) -> Result<u32, ()> {
    sleep(Duration::new(2, 0));
    Ok(a * b)
}

pub fn main() {
    println!("\n ** default cache with default name **");
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

    println!("\n ** default cache with explicit name **");
    fib_2(3);
    fib_2(3);
    {
        let cache = FLIB.lock().unwrap();
        println!("hits: {:?}", cache.cache_hits());
        println!("misses: {:?}", cache.cache_misses());
        // make sure lock is dropped
    }

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
    slow(10, 10);
    println!(" - second run `slow(10)`");
    slow(10, 10);
    {
        let cache = SLOW.lock().unwrap();
        println!("hits: {:?}", cache.cache_hits());
        println!("misses: {:?}", cache.cache_misses());
        // make sure the cache-lock is dropped
    }

    println!("\n ** slow result func **");
    println!(" - first run `slow_result(10)`");
    let _ = slow_result(10, 10);
    println!(" - second run `slow_result(10)`");
    let _ = slow_result(10, 10);
    {
        let cache = SLOW_RESULT.lock().unwrap();
        println!("hits: {:?}", cache.cache_hits());
        println!("misses: {:?}", cache.cache_misses());
        // make sure the cache-lock is dropped
    }
}
