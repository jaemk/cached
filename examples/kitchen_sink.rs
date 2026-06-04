/*
"Kitchen sink": the default `UnboundCache`, an explicit `ty` + `create` store,
and `#[cached(max_size = N)]` (LRU), with direct `Cached`-trait access to the
generated cache statics.

Run:
    cargo run --example kitchen_sink --features "time_stores,proc_macro"
*/

use cached::macros::cached;
use cached::{Cached, LruCache, UnboundCache};
use std::cmp::Eq;
use std::collections::HashMap;
use std::hash::Hash;
use std::thread::sleep;
use std::time::Duration;

// cached shorthand, uses the default unbounded cache.
// Equivalent to specifying `FIB: UnboundCache<(u32), u32> = UnboundCache::builder().build().unwrap();`
#[cached]
fn fib(n: u32) -> u32 {
    if n == 0 || n == 1 {
        return n;
    }
    fib(n - 1) + fib(n - 2)
}

// Same as above, but preallocates some space.
// Note that the cache key type is a tuple of function argument types.
#[cached(
    ty = "UnboundCache<u32, u32>",
    create = "{ UnboundCache::builder().capacity(50).build().unwrap() }"
)]
fn fib_specific(n: u32) -> u32 {
    if n == 0 || n == 1 {
        return n;
    }
    fib_specific(n - 1) + fib_specific(n - 2)
}

// Specify a specific cache type
// Note that the cache key type is a tuple of function argument types.
#[cached(max_size = 100)]
fn slow(a: u32, b: u32) -> u32 {
    sleep(Duration::new(2, 0));
    a * b
}

// Specify a specific cache type and an explicit key expression
// Note that the cache key type is a `String` created from the borrow arguments
#[cached(
    ty = "LruCache<String, usize>",
    create = "{ LruCache::builder().max_size(100).build().unwrap() }",
    convert = r#"{ format!("{a}{b}") }"#
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
    fn cache_get<Q>(&mut self, k: &Q) -> Option<&V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.store.get(k)
    }
    fn cache_get_mut<Q>(&mut self, k: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.store.get_mut(k)
    }
    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, k: K, f: F) -> &mut V {
        self.store.entry(k).or_insert_with(f)
    }
    fn cache_try_get_or_set_with<F: FnOnce() -> Result<V, E>, E>(
        &mut self,
        k: K,
        f: F,
    ) -> Result<&mut V, E> {
        use std::collections::hash_map::Entry;
        let v = match self.store.entry(k) {
            Entry::Occupied(occupied) => occupied.into_mut(),
            Entry::Vacant(vacant) => vacant.insert(f()?),
        };

        Ok(v)
    }
    fn cache_set(&mut self, k: K, v: V) -> Option<V> {
        self.store.insert(k, v)
    }
    fn cache_remove<Q>(&mut self, k: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.store.remove(k)
    }
    fn cache_remove_entry<Q>(&mut self, k: &Q) -> Option<(K, V)>
    where
        K: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.store.remove_entry(k)
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
#[cached(
    name = "CUSTOM",
    ty = "MyCache<u32, ()>",
    create = "{ MyCache::with_capacity(50) }"
)]
fn custom(n: u32) {
    if n == 0 {
        return;
    }
    custom(n - 1);
}

#[cached(ttl = 1)]
fn expires(a: i32) -> i32 {
    a
}

#[cached(ttl = 1)]
fn expires_result(a: i32) -> Result<i32, ()> {
    Ok(a)
}

#[cached(ttl = 1)]
fn expires_option(a: i32) -> Option<i32> {
    Some(a)
}

#[cached(ttl = 1, name = "EXPIRES_FOR_PRIMING")]
fn expires_for_priming(a: i32) -> i32 {
    a
}

pub fn main() {
    println!("\n ** default cache **");
    fib(3);
    fib(3);
    {
        let cache = FIB.read();
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
        let cache = FIB_SPECIFIC.read();
        println!("hits: {:?}", cache.cache_hits());
        println!("misses: {:?}", cache.cache_misses());
        // make sure lock is dropped
    }
    fib_specific(20);
    fib_specific(20);

    println!("\n ** custom cache **");
    custom(25);
    {
        let cache = CUSTOM.read();
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
        let cache = SLOW.read();
        println!("hits: {:?}", cache.cache_hits());
        println!("misses: {:?}", cache.cache_misses());
        // make sure the cache-lock is dropped
    }

    println!("\n ** expires **");
    expires(1);
    expires(1);
    expires(2);
    sleep(Duration::new(2, 0));
    expires(1);
    {
        let cache = EXPIRES.read();
        println!("hits: {:?}", cache.cache_hits());
        println!("misses: {:?}", cache.cache_misses());
    }

    println!("\n ** expires_result **");
    let _ = expires_result(1);
    let _ = expires_result(1);
    let _ = expires_result(2);
    sleep(Duration::new(2, 0));
    let _ = expires_result(1);
    {
        let cache = EXPIRES_RESULT.read();
        println!("hits: {:?}", cache.cache_hits());
        println!("misses: {:?}", cache.cache_misses());
    }

    println!("\n ** expires_option **");
    expires_option(1);
    expires_option(1);
    expires_option(2);
    sleep(Duration::new(2, 0));
    expires_option(1);
    {
        let cache = EXPIRES_OPTION.read();
        println!("hits: {:?}", cache.cache_hits());
        println!("misses: {:?}", cache.cache_misses());
    }

    println!("\n ** expires_for_priming **");
    // First call: cache miss, result gets computed and cached
    expires_for_priming(1);
    // Second call: cache hit (key 1 is still within its TTL)
    expires_for_priming(1);
    // Prime key 1 and key 2 — refreshes the cache directly without affecting hit/miss counters
    expires_for_priming_prime_cache(1);
    expires_for_priming_prime_cache(2);
    {
        let c = EXPIRES_FOR_PRIMING.read();
        // Only the two explicit function calls above count toward metrics
        assert_eq!(c.cache_hits(), Some(1)); // second call was a hit
        assert_eq!(c.cache_misses(), Some(1)); // first call was a miss
    }
    // Sleep longer than the 1-second TTL so the cached values expire
    sleep(Duration::new(2, 0));
    // Re-prime key 1 so it's fresh in the cache again
    expires_for_priming_prime_cache(1);
    // Now calling the function finds the freshly-primed value — it's a hit
    assert_eq!(expires_for_priming(1), 1);
    {
        let c = EXPIRES_FOR_PRIMING.read();
        assert_eq!(c.cache_hits(), Some(2)); // this last call was also a hit
        assert_eq!(c.cache_misses(), Some(1)); // still only 1 miss
    }

    println!("done!");
}
