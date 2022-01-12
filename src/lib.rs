/*!
[![Build Status](https://travis-ci.org/jaemk/cached.svg?branch=master)](https://travis-ci.org/jaemk/cached)
[![crates.io](https://img.shields.io/crates/v/cached.svg)](https://crates.io/crates/cached)
[![docs](https://docs.rs/cached/badge.svg)](https://docs.rs/cached)

> Caching structures and simplified function memoization

`cached` provides implementations of several caching structures as well as a handy macro
for defining memoized functions.

Memoized functions defined using `#[cached]`/`#[once]`/`cached!` macros are thread-safe with the backing function-cache wrapped in a mutex/rwlock.
By default, the function-cache is **not** locked for the duration of the function's execution, so initial (on an empty cache)
concurrent calls of long-running functions with the same arguments will each execute fully and each overwrite
the memoized value as they complete. This mirrors the behavior of Python's `functools.lru_cache`. To synchronize the execution and caching
of un-cached arguments, specify `#[cached(sync_writes = true)]` /
`#[once(sync_writes = true)]`.

See [`cached::stores` docs](https://docs.rs/cached/latest/cached/stores/index.html) for details about the
cache stores available.

**Features**

- `proc_macro`: (default) pull in proc macro support
- `async`: (default) Add `CachedAsync` trait

## Defining memoized functions using macros, `#[cached]`, `#[once]`, & `cached!`

**Note**:

> It is recommended you use the two proc-macros (`#[cached]`, `#[once]`) as
> these work with async functions and have more options/features. See the `examples/`
> directory for more sample usage, and `cached_proc_macro/src/lib.rs` for the
> full list of available proc-macro arguments.
>
> The declarative macros (`cached!` and co.) are still available, but are less maintained
> and have fewer features.

The basic usage looks like:


```rust,no_run
use cached::proc_macro::cached;

/// Defines a function named `fib` that uses a cache implicitly named `FIB`.
/// By default, the cache will be the function's in all caps.
/// The following line is equivalent to #[cached(name = "FIB", unbound)]
#[cached]
fn fib(n: u64) -> u64 {
    if n == 0 || n == 1 { return n }
    fib(n-1) + fib(n-2)
}
# pub fn main() { }
```

```rust,no_run
use std::thread::sleep;
use std::time::Duration;
use cached::proc_macro::cached;

/// Use an lru cache with size 100 and a `(String, String)` cache key
#[cached(size=100)]
fn keyed(a: String, b: String) -> usize {
    let size = a.len() + b.len();
    sleep(Duration::new(size as u64, 0));
    size
}
# pub fn main() { }
```

```rust,no_run
use std::thread::sleep;
use std::time::Duration;
use cached::proc_macro::cached;

/// Use a timed-lru cache with size 1, a TTL of 60s,
/// and a `(usize, usize)` cache key
#[cached(size=1, time=60)]
fn keyed(a: usize, b: usize) -> usize {
    let total = a + b;
    sleep(Duration::new(total as u64, 0));
    total
}
pub fn main() {
    keyed(1, 2);  // Not cached, will sleep (1+2)s

    keyed(1, 2);  // Cached, no sleep

    sleep(Duration::new(60, 0));  // Sleep for the TTL

    keyed(1, 2);  // 60s TTL has passed so the cached
                  // value has expired, will sleep (1+2)s

    keyed(1, 2);  // Cached, no sleep

    keyed(2, 1);  // New args, not cached, will sleep (2+1)s

    keyed(1, 2);  // Was evicted because of lru size of 1,
                  // will sleep (1+2)s
}
```

```rust,no_run
use std::thread::sleep;
use std::time::Duration;
use cached::proc_macro::cached;

/// Use a timed cache with a TTL of 60s
/// that refreshes the entry TTL on cache hit,
/// and a `(String, String)` cache key
#[cached(time=60, time_refresh=true)]
fn keyed(a: String, b: String) -> usize {
    let size = a.len() + b.len();
    sleep(Duration::new(size as u64, 0));
    size
}
# pub fn main() { }
```

```rust,no_run
use cached::proc_macro::cached;

# fn do_something_fallible() -> std::result::Result<(), ()> {
#     Ok(())
# }

/// Cache a fallible function. Only `Ok` results are cached.
#[cached(size=1, result = true)]
fn keyed(a: String) -> Result<usize, ()> {
    do_something_fallible()?;
    Ok(a.len())
}
# pub fn main() { }
```

```rust,no_run
use cached::proc_macro::cached;

/// Cache an optional function. Only `Some` results are cached.
#[cached(size=1, option = true)]
fn keyed(a: String) -> Option<usize> {
    if a == "a" {
        Some(a.len())
    } else {
        None
    }
}
# pub fn main() { }
```

```rust,no_run
use cached::proc_macro::cached;

/// Cache an optional function. Only `Some` results are cached.
/// When called concurrently, duplicate argument-calls will be
/// synchronized so as to only run once - the remaining concurrent
/// calls return a cached value.
#[cached(size=1, option = true, sync_writes = true)]
fn keyed(a: String) -> Option<usize> {
    if a == "a" {
        Some(a.len())
    } else {
        None
    }
}
# pub fn main() { }
```

```rust,no_run
use cached::proc_macro::cached;
use cached::Return;

/// Get a `cached::Return` value that indicates
/// whether the value returned came from the cache:
/// `cached::Return.was_cached`.
/// Use an LRU cache and a `String` cache key.
#[cached(size=1, with_cached_flag = true)]
fn calculate(a: String) -> Return<String> {
    Return::new(a)
}
pub fn main() {
    let r = calculate("a".to_string());
    assert!(!r.was_cached);
    let r = calculate("a".to_string());
    assert!(r.was_cached);
    // Return<String> derefs to String
    assert_eq!(r.to_uppercase(), "A");
}
```

```rust,no_run
use cached::proc_macro::cached;
use cached::Return;

# fn do_something_fallible() -> std::result::Result<(), ()> {
#     Ok(())
# }

/// Same as the previous, but returning a Result
#[cached(size=1, result = true, with_cached_flag = true)]
fn calculate(a: String) -> Result<Return<usize>, ()> {
    do_something_fallible()?;
    Ok(Return::new(a.len()))
}
pub fn main() {
    match calculate("a".to_string()) {
        Err(e) => eprintln!("error: {:?}", e),
        Ok(r) => {
            println!("value: {:?}, was cached: {}", *r, r.was_cached);
            // value: "a", was cached: true
        }
    }
}
```

```rust,no_run
use cached::proc_macro::cached;
use cached::Return;

/// Same as the previous, but returning an Option
#[cached(size=1, option = true, with_cached_flag = true)]
fn calculate(a: String) -> Option<Return<usize>> {
    if a == "a" {
        Some(Return::new(a.len()))
    } else {
        None
    }
}
pub fn main() {
    if let Some(a) = calculate("a".to_string()) {
        println!("value: {:?}, was cached: {}", *a, a.was_cached);
        // value: "a", was cached: true
    }
}
```

```rust,no_run
use std::thread::sleep;
use std::time::Duration;
use cached::proc_macro::cached;
use cached::SizedCache;

/// Use an explicit cache-type with a custom creation block and custom cache-key generating block
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
# pub fn main() { }
```

```rust,no_run
use cached::proc_macro::once;

/// Only cache the initial function call.
/// Function will be re-executed after the cache
/// expires (according to `time` seconds).
/// When no (or expired) cache, concurrent calls
/// will synchronize (`sync_writes`) so the function
/// is only executed once.
#[once(time=10, option = true, sync_writes = true)]
fn keyed(a: String) -> Option<usize> {
    if a == "a" {
        Some(a.len())
    } else {
        None
    }
}
# pub fn main() { }
```

```rust
use std::thread::sleep;
use std::time::Duration;
use cached::proc_macro::cached;

/// Use a timed cache with a TTL of 60s.
/// Run a background thread to continuously refresh a specific key.
#[cached(time = 60, key = "String", convert = r#"{ String::from(a) }"#)]
fn keyed(a: &str) -> usize {
    a.len()
}
pub fn main() {
    std::thread::spawn(|| {
        loop {
            sleep(Duration::from_secs(60));
            keyed_prime_cache("a");
        }
    });
}
```

```rust
use std::thread::sleep;
use std::time::Duration;
use cached::proc_macro::once;

/// Run a background thread to continuously refresh a singleton.
#[once]
fn keyed() -> String {
    // do some long http request
    "some data".to_string()
}
pub fn main() {
    std::thread::spawn(|| {
        loop {
            sleep(Duration::from_secs(60));
            keyed_prime_cache();
        }
    });
}
```

```rust
use std::thread::sleep;
use std::time::Duration;
use cached::proc_macro::cached;

/// Run a background thread to continuously refresh every key of a cache
#[cached(key = "String", convert = r#"{ String::from(a) }"#)]
fn keyed(a: &str) -> usize {
    a.len()
}
pub fn main() {
    std::thread::spawn(|| {
        loop {
            sleep(Duration::from_secs(60));
            let keys: Vec<String> = {
                // note the cache keys are a tuple of all function arguments, unless it's one value
                KEYED.lock().unwrap().get_store().keys().map(|k| k.clone()).collect()
            };
            for k in &keys {
                keyed_prime_cache(k);
            }
        }
    });
}
```

----


`#[cached]`/`cached!` defined functions will have their results cached using the function's arguments as a key
(or a specific expression when using `cached_key!`).
When a `cached!` defined function is called, the function's cache is first checked for an already
computed (and still valid) value before evaluating the function body.

Due to the requirements of storing arguments and return values in a global cache:

- Function return types must be owned and implement `Clone`
- Function arguments must either be owned and implement `Hash + Eq + Clone` OR the `cached_key!`
  macro must be used to convert arguments into an owned + `Hash + Eq + Clone` type.
- Arguments and return values will be `cloned` in the process of insertion and retrieval.
- `#[cached]`/`cached!` functions should not be used to produce side-effectual results!
- `#[cached]`/`cached!` functions cannot live directly under `impl` blocks since `cached!` expands to a
  `once_cell` initialization and a function definition.
- `#[cached]`/`cached!` functions cannot accept `Self` types as a parameter.

**NOTE**: Any custom cache that implements `cached::Cached` can be used with the `cached` macros in place of the built-ins.

See [`examples`](https://github.com/jaemk/cached/tree/master/examples) for basic usage of proc-macro &
macro-rules macros and an example of implementing a custom cache-store.


### `cached!` and `cached_key!` Usage & Options:

There are several options depending on how explicit you want to be. See below for a full syntax breakdown.


1.) Using the shorthand will use an unbounded cache.


```rust,no_run
#[macro_use] extern crate cached;

/// Defines a function named `fib` that uses a cache named `FIB`
cached!{
    FIB;
    fn fib(n: u64) -> u64 = {
        if n == 0 || n == 1 { return n }
        fib(n-1) + fib(n-2)
    }
}
# pub fn main() { }
```


2.) Using the full syntax requires specifying the full cache type and providing
    an instance of the cache to use. Note that the cache's key-type is a tuple
    of the function argument types. If you would like fine grained control over
    the key, you can use the `cached_key!` macro.
    The following example uses a `SizedCache` (LRU):

```rust,no_run
#[macro_use] extern crate cached;

use std::thread::sleep;
use std::time::Duration;
use cached::SizedCache;

/// Defines a function `compute` that uses an LRU cache named `COMPUTE` which has a
/// size limit of 50 items. The `cached!` macro will implicitly combine
/// the function arguments into a tuple to be used as the cache key.
cached!{
    COMPUTE: SizedCache<(u64, u64), u64> = SizedCache::with_size(50);
    fn compute(a: u64, b: u64) -> u64 = {
        sleep(Duration::new(2, 0));
        return a * b;
    }
}
# pub fn main() { }
```


3.) The `cached_key` macro functions identically, but allows you to define the
    cache key as an expression.

```rust,no_run
#[macro_use] extern crate cached;

use std::thread::sleep;
use std::time::Duration;
use cached::SizedCache;

/// Defines a function named `length` that uses an LRU cache named `LENGTH`.
/// The `Key = ` expression is used to explicitly define the value that
/// should be used as the cache key. Here the borrowed arguments are converted
/// to an owned string that can be stored in the global function cache.
cached_key!{
    LENGTH: SizedCache<String, usize> = SizedCache::with_size(50);
    Key = { format!("{}{}", a, b) };
    fn length(a: &str, b: &str) -> usize = {
        let size = a.len() + b.len();
        sleep(Duration::new(size as u64, 0));
        size
    }
}
# pub fn main() { }
```

4.) The `cached_result` and `cached_key_result` macros function similarly to `cached`
    and `cached_key` respectively but the cached function needs to return `Result`
    (or some type alias like `io::Result`). If the function returns `Ok(val)` then `val`
    is cached, but errors are not. Note that only the success type needs to implement
    `Clone`, _not_ the error type. When using `cached_result` and `cached_key_result`,
    the cache type cannot be derived and must always be explicitly specified.

```rust,no_run
#[macro_use] extern crate cached;

use cached::UnboundCache;

/// Cache the successes of a function.
/// To use `cached_key_result` add a key function as in `cached_key`.
cached_result!{
   MULT: UnboundCache<(u64, u64), u64> = UnboundCache::new(); // Type must always be specified
   fn mult(a: u64, b: u64) -> Result<u64, ()> = {
        if a == 0 || b == 0 {
            return Err(());
        } else {
            return Ok(a * b);
        }
   }
}
# pub fn main() { }
```


## Syntax

The common macro syntax is:


```rust,ignore
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


# Fine grained control using `cached_control!`

The `cached_control!` macro allows you to provide expressions that get plugged into key areas
of the memoized function. While the `cached` and `cached_result` variants are adequate for most
scenarios, it can be useful to have the ability to customize the macro's functionality.

```rust,no_run
#[macro_use] extern crate cached;

use cached::UnboundCache;

/// The following usage plugs in expressions to make the macro behave like
/// the `cached_result!` macro.
cached_control!{
    CACHE: UnboundCache<String, String> = UnboundCache::new();

    // Use an owned copy of the argument `input` as the cache key
    Key = { input.to_owned() };

    // If a cached value exists, it will bind to `cached_val` and
    // a `Result` will be returned containing a copy of the cached
    // evaluated body. This will return before the function body
    // is executed.
    PostGet(cached_val) = { return Ok(cached_val.clone()) };

    // The result of executing the function body will be bound to
    // `body_result`. In this case, the function body returns a `Result`.
    // We match on the `Result`, returning an early `Err` if the function errored.
    // Otherwise, we pass on the function's result to be cached.
    PostExec(body_result) = {
        match body_result {
            Ok(v) => v,
            Err(e) => return Err(e),
        }
    };

    // When inserting the value into the cache we bind
    // the to-be-set-value to `set_value` and give back a copy
    // of it to be inserted into the cache
    Set(set_value) = { set_value.clone() };

    // Before returning, print the value that will be returned
    Return(return_value) = {
        println!("{}", return_value);
        Ok(return_value)
    };

    fn can_fail(input: &str) -> Result<String, String> = {
        let len = input.len();
        if len < 3 { Ok(format!("{}-{}", input, len)) }
        else { Err("too big".to_string()) }
    }
}
# pub fn main() {}
```

*/

pub extern crate once_cell;

mod lru_list;
pub mod macros;
pub mod stores;

pub use stores::{SizedCache, TimedCache, TimedSizedCache, UnboundCache};

#[cfg(feature = "proc_macro")]
pub mod proc_macro {
    pub use cached_proc_macro::cached;
    pub use cached_proc_macro::once;
    pub use cached_proc_macro_types::Return;
}
#[cfg(feature = "proc_macro")]
pub use async_mutex;
#[cfg(feature = "proc_macro")]
pub use async_rwlock;
#[cfg(feature = "proc_macro")]
pub use proc_macro::Return;

#[cfg(feature = "async")]
use {async_trait::async_trait, futures::Future};

/// Cache operations
pub trait Cached<K, V> {
    /// Attempt to retrieve a cached value
    fn cache_get(&mut self, k: &K) -> Option<&V>;

    /// Attempt to retrieve a cached value with mutable access
    fn cache_get_mut(&mut self, k: &K) -> Option<&mut V>;

    /// Insert a key, value pair and return the previous value
    fn cache_set(&mut self, k: K, v: V) -> Option<V>;

    /// Get or insert a key, value pair
    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, k: K, f: F) -> &mut V;

    /// Remove a cached value
    fn cache_remove(&mut self, k: &K) -> Option<V>;

    /// Remove all cached values. Keeps the allocated memory for reuse.
    fn cache_clear(&mut self);

    /// Remove all cached values. Free memory and return to initial state
    fn cache_reset(&mut self);

    /// Reset misses/hits counters
    fn cache_reset_metrics(&mut self) {}

    /// Return the current cache size (number of elements)
    fn cache_size(&self) -> usize;

    /// Return the number of times a cached value was successfully retrieved
    fn cache_hits(&self) -> Option<u64> {
        None
    }

    /// Return the number of times a cached value was unable to be retrieved
    fn cache_misses(&self) -> Option<u64> {
        None
    }

    /// Return the cache capacity
    fn cache_capacity(&self) -> Option<usize> {
        None
    }

    /// Return the lifespan of cached values (time to eviction)
    fn cache_lifespan(&self) -> Option<u64> {
        None
    }

    /// Set the lifespan of cached values, returns the old value
    fn cache_set_lifespan(&mut self, _seconds: u64) -> Option<u64> {
        None
    }
}

#[cfg(feature = "async")]
#[async_trait]
pub trait CachedAsync<K, V> {
    async fn get_or_set_with<F, Fut>(&mut self, k: K, f: F) -> &mut V
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = V> + Send;

    async fn try_get_or_set_with<F, Fut, E>(&mut self, k: K, f: F) -> Result<&mut V, E>
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<V, E>> + Send;
}
