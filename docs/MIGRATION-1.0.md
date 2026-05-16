# Migrating to `cached` 1.0

This guide walks through every breaking change between the pre-1.0 releases
(`0.x`) and `cached` 1.0. It is written for humans: each section explains *what*
changed, *why* it changed, and *how* to update your code, with before/after
examples.

If you want a terse, mechanical checklist optimized for automated find-and-replace
(or for handing to an AI assistant), see
[`MIGRATION-1.0-AGENT.md`](./MIGRATION-1.0-AGENT.md).

> **TL;DR** — The biggest changes are: cache stores were renamed for clarity
> (`SizedCache` → `LruCache`, `TimedCache` → `TtlCache`, …), the declarative
> `cached!` macros were removed in favor of the procedural macros, the
> `cached::proc_macro` module is now `cached::macros`, macro/builder attributes
> were renamed (`time` → `ttl`, `time_refresh` → `refresh`, `set_*` prefixes
> dropped), and **the Redis cache key format changed** — existing Redis caches
> will cold-start after upgrading.

---

## 1. Cache store renames

The store types were renamed so the name describes the *eviction policy*, not an
implementation detail. The behavior of each store is unchanged — only the name.

| Pre-1.0 | 1.0 | Module (pre-1.0 → 1.0) |
|---|---|---|
| `SizedCache` | `LruCache` | `stores::sized` → `stores::lru` |
| `TimedCache` | `TtlCache` | `stores::timed` → `stores::ttl` |
| `TimedSizedCache` | `LruTtlCache` | `stores::timed_sized` → `stores::lru_ttl` |
| `ExpiringSizedCache` | `TtlSortedCache` | `stores::expiring_sized` → `stores::ttl_sorted` |
| `ExpiringValueCache` | `ExpiringLruCache` | `stores::expiring_value_cache` → `stores::expiring_lru` |
| `UnboundCache` | `UnboundCache` *(unchanged)* | `stores::unbound` *(unchanged)* |

```rust
// Before
use cached::{SizedCache, TimedCache, TimedSizedCache};
use cached::stores::ExpiringSizedCache;

// After
use cached::{LruCache, TtlCache, LruTtlCache};
use cached::stores::TtlSortedCache;
```

### Constructor method renames

Along with the type renames, the "lifespan" vocabulary became "ttl", and the
deprecated `with_capacity` shim was removed:

| Pre-1.0 | 1.0 |
|---|---|
| `SizedCache::with_capacity(n)` | `LruCache::with_size(n)` |
| `SizedCache::with_size(n)` | `LruCache::with_size(n)` |
| `TimedCache::with_lifespan(ttl)` | `TtlCache::with_ttl(ttl)` |
| `TimedCache::with_lifespan_and_capacity(ttl, n)` | `TtlCache::with_ttl_and_capacity(ttl, n)` |
| `TimedCache::with_lifespan_and_refresh(ttl, r)` | `TtlCache::with_ttl_and_refresh(ttl, r)` |
| `TimedSizedCache::with_size_and_lifespan(n, ttl)` | `LruTtlCache::with_size_and_ttl(n, ttl)` |
| `TimedSizedCache::with_size_and_lifespan_and_refresh(n, ttl, r)` | `LruTtlCache::with_size_and_ttl_and_refresh(n, ttl, r)` |
| `TtlSortedCache::ttl_millis(...)` *(deprecated)* | use `TtlSortedCache::new(Duration)` |

```rust
// Before
let c = TimedCache::with_lifespan_and_refresh(Duration::from_secs(60), true);

// After
let c = TtlCache::with_ttl_and_refresh(Duration::from_secs(60), true);
```

Every in-memory store also gained a `::builder()` API in 1.0, which is the
recommended way to construct stores going forward (it supports `on_evict`
callbacks and other new options):

```rust
let c = TtlCache::builder()
    .ttl(Duration::from_secs(60))
    .refresh(true)
    .build();
```

### Refresh-accessor renames

The refresh accessors were renamed to `*_on_hit` to make their meaning explicit
(they control whether reading an entry resets its TTL):

| Pre-1.0 | 1.0 |
|---|---|
| `TimedCache::refresh()` | `TtlCache::refresh_on_hit()` |
| `TimedCache::set_refresh(b)` | `TtlCache::set_refresh_on_hit(b)` |
| `TimedSizedCache::refresh()` | `LruTtlCache::refresh_on_hit()` |
| `TimedSizedCache::set_refresh(b)` | `LruTtlCache::set_refresh_on_hit(b)` |

### Store-accessor rename (`get_store` → `store`)

Following the Rust API Guidelines getter convention (C-GETTER), `get_store()`
was renamed to `store()` on `TtlCache`, `LruTtlCache`, and `UnboundCache`:

```rust
// Before
let inner = cache.get_store();
// After
let inner = cache.store();
```

### `TtlSortedCache::get_borrowed` removed

`ExpiringSizedCache::get_borrowed` is gone. `TtlSortedCache`'s reads (via the
trait short alias `Cached::get` or `CachedRead::cache_get_read`) are now
generic over borrowed keys, so the separate method is no longer needed:

```rust
// Before
let v = cache.get_borrowed("key");
// After — accepts anything the key borrows as
let v = cache.get("key");
let v = cache.get(&some_slice[..]);
```

### `TtlSortedCache` inherent `remove` / `clear` / `len` / `is_empty` / `get` removed

These five inherent methods shadowed the same-named [`Cached`] short aliases
(and one of them, `get`, had a subtle behavior difference — see below). They
have been removed; bring the appropriate trait into scope:

```rust
use cached::{Cached, CachedRead}; // (or CachedPeek for non-metric reads)

// Before — inherent methods
cache.remove(&k);
cache.clear();
cache.len();
cache.is_empty();
cache.get(&k);          // &self, did NOT evict expired entries on access

// After — trait short aliases (Cached) for the &mut self path
cache.remove(&k);       // Cached::remove<Q> (Q via Borrow), same behavior
cache.clear();          // Cached::clear, same behavior
cache.len();            // Cached::len, same behavior
cache.is_empty();       // Cached::is_empty, same behavior
cache.get(&k);          // now &mut self; DELEGATES to cache_get, which
                        // DOES remove expired entries on access in this store
```

The `get` change is the only one with a semantic shift. **If you relied on
the inherent `get`'s `&self` non-evicting behavior** (e.g. behind a shared
lock guard like `RwLockReadGuard`), switch to one of the non-mutating reads:

```rust
use cached::CachedRead;          // hit/miss metrics, &self, non-evicting
let v = cache.cache_get_read(&k);

use cached::CachedPeek;          // no metrics, &self, non-evicting
let v = cache.cache_peek(&k);
```

This unblocks the `&TtlSortedCache` (shared-borrow) read pattern that the
inherent `get` previously served; the new path is via the dedicated
`CachedRead` / `CachedPeek` traits already implemented by the store.

---

## 2. `CanExpire` trait renamed to `Expires`

The trait implemented by values stored in `ExpiringLruCache` (formerly
`ExpiringValueCache`) was renamed:

```rust
// Before
use cached::CanExpire;
impl CanExpire for MyValue {
    fn is_expired(&self) -> bool { /* ... */ }
}
fn store<V: CanExpire>(v: V) { /* ... */ }

// After
use cached::Expires;
impl Expires for MyValue {
    fn is_expired(&self) -> bool { /* ... */ }
}
fn store<V: Expires>(v: V) { /* ... */ }
```

Update both the `use` import and every `V: CanExpire` bound to `V: Expires`.

---

## 3. Declarative macros removed

The declarative (`macro_rules!`) macros were removed entirely:

- `cached!`
- `cached_key!`
- `cached_result!`
- `cached_key_result!`
- `cached_control!`

Use the procedural macros instead — they are strictly more capable (async
support, `result`/`option` handling, `sync_writes`, etc.):

```rust
// Before
cached! {
    FIB;
    fn fib(n: u64) -> u64 = {
        if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
    }
}

// After
use cached::macros::cached;

#[cached]
fn fib(n: u64) -> u64 {
    if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
}
```

For the `cached_key!` / `cached_key_result!` variants, use the `key` + `convert`
attributes on `#[cached]`. For `cached_control!`, use `#[cached]` with `result`
/ `option` or a custom store.

---

## 4. Proc-macro module renamed: `proc_macro` → `macros`

The procedural macros are now re-exported from `cached::macros` instead of
`cached::proc_macro`:

```rust
// Before
use cached::proc_macro::cached;
use cached::proc_macro::once;
use cached::proc_macro::io_cached;

// After
use cached::macros::cached;
use cached::macros::once;
use cached::macros::concurrent_cached; // also renamed from `io_cached` — see §12
```

> Note: the **Cargo feature flag** is still named `proc_macro` — only the Rust
> module path changed. There is no longer a separate `macros` module of
> declarative macros (those were removed, see §3), so `cached::macros` now
> unambiguously means the proc-macro re-exports. The `io_cached` macro was also
> *renamed* to `concurrent_cached` (separate from this module move) — see §12.

---

## 5. Macro attribute renames

`#[cached]`, `#[once]`, and `#[concurrent_cached]` (pre-1.0 `#[io_cached]`, see
§12) each renamed their `time` attribute. `#[cached]` and `#[concurrent_cached]`
also renamed `time_refresh`; `#[once]` has never supported a refresh attribute
(it holds a single cached value, so refresh-on-hit is not applicable):

| Pre-1.0 attribute | 1.0 attribute | Applies to |
|---|---|---|
| `time = N` | `ttl = N` | `#[cached]`, `#[once]`, `#[concurrent_cached]` |
| `time_refresh = <bool>` | `refresh = <bool>` | `#[cached]`, `#[concurrent_cached]` only |

```rust
// Before
#[cached(time = 60, time_refresh = true)]
fn slow(x: u32) -> u32 { /* ... */ }

// After
#[cached(ttl = 60, refresh = true)]
fn slow(x: u32) -> u32 { /* ... */ }
```

The macros emit a dedicated compile error if you use the old `time` /
`time_refresh` names, so the compiler will point you at exactly what to change.

---

## 6. Builder method renames (`set_` prefix dropped)

The IO-store builders dropped the `set_` prefix to match the in-memory builder
style. The deprecated `set_lifespan` shim on `DiskCacheBuilder` was also removed.

### `DiskCacheBuilder`

| Pre-1.0 | 1.0 |
|---|---|
| `set_ttl(d)` | `ttl(d)` |
| `set_refresh(b)` | `refresh(b)` |
| `set_disk_directory(p)` | `disk_directory(p)` |
| `set_sync_to_disk_on_cache_change(b)` | `sync_to_disk_on_cache_change(b)` |
| `set_connection_config(c)` | `connection_config(c)` |
| `set_lifespan(d)` *(deprecated)* | `ttl(d)` |

### `RedisCacheBuilder` / `AsyncRedisCacheBuilder`

| Pre-1.0 | 1.0 |
|---|---|
| `set_lifespan(d)` | `ttl(d)` |
| `set_refresh(b)` | `refresh(b)` |
| `set_namespace(s)` | `namespace(s)` |
| `set_prefix(s)` | `prefix(s)` |
| `set_connection_string(s)` | `connection_string(s)` |
| `set_connection_pool_max_size(n)` | `connection_pool_max_size(n)` |
| `set_connection_pool_min_idle(n)` | `connection_pool_min_idle(n)` |
| `set_connection_pool_max_lifetime(d)` | `connection_pool_max_lifetime(d)` |
| `set_connection_pool_idle_timeout(d)` | `connection_pool_idle_timeout(d)` |
| `set_client_side_caching(b)` *(async only)* | `client_side_caching(b)` |

The internal connection-string resolver was renamed `connection_string` →
`resolve_connection_string` so the bare `connection_string` name is now the
public setter.

```rust
// Before
let cache = RedisCacheBuilder::new("prefix", 60)
    .set_namespace("myapp:")
    .set_refresh(true)
    .build()?;

// After
let cache = RedisCacheBuilder::new("prefix", 60)
    .namespace("myapp:")
    .refresh(true)
    .build()?;
```

> Note — builder-shape inconsistency (not changing in 1.0): the in-memory and
> I/O stores reach a builder differently, and this is deliberate but worth
> knowing. In-memory: `LruCache::builder()` → `…build()` returns the **store**
> (infallible), with a separate fallible `try_build()` → `Result<_, BuildError>`.
> I/O-backed: `DiskCache::new(name)` / `RedisCache::new(prefix, ttl)` return a
> **builder**, and that builder's `build()` is the fallible one
> (`Result<_, DiskCacheBuildError>` / `Result<_, RedisCacheBuildError>`). So
> `XCache::new(..)` yields a store for in-memory but a builder for disk/redis,
> and the fallible constructor is `try_build` for in-memory but `build` for
> disk/redis. These remain distinct in 1.0; match the table/examples above for
> the store you are constructing.

---

## 7. TTL/refresh method renames

The TTL/refresh methods were renamed across all cache types to unify around the
`CacheTtl` vocabulary. This includes the `ConcurrentCached`/`ConcurrentCachedAsync`
trait methods **and** the `cache_ttl`/`cache_set_ttl`/`cache_unset_ttl` override
hooks that were previously on the `Cached` trait — those three have been removed
entirely; use `CacheTtl` for timed stores.

| Pre-1.0 | 1.0 |
|---|---|
| `cache_ttl()` | `ttl()` |
| `cache_set_ttl(d)` | `set_ttl(d)` |
| `cache_unset_ttl()` | `unset_ttl()` |
| `cache_set_refresh(b)` | `set_refresh_on_hit(b)` |

```rust
// Before
cache.cache_set_ttl(Duration::from_secs(30));
// After
cache.set_ttl(Duration::from_secs(30));
```

> Behavioral note for Redis stores: `set_ttl` only affects entries inserted
> *after* the call — existing Redis keys keep whatever TTL they were stored
> with. `unset_ttl` is a no-op on Redis (Redis cached entries always require a
> TTL) and always returns `None`.

> `set_ttl` returns `Option<Duration>` (the previous TTL) uniformly across all
> timed stores, including `TtlSortedCache`'s inherent method, so the return type
> does not depend on which store you call it on.

> These methods come from a trait, so the trait must be in scope at the call
> site: `use cached::CacheTtl;` for the timed in-memory stores (`TtlCache`,
> `LruTtlCache`, `TtlSortedCache`), or
> `use cached::{ConcurrentCached, ConcurrentCachedAsync};` for
> `DiskCache`/`RedisCache`/`AsyncRedisCache`. A bare `cache.set_ttl(..)` that
> compiled in 0.x via an inherent method may now need the trait imported.

### `CachedAsync` get-or-set method renames

The two `CachedAsync` get-or-set methods are now `async_`-prefixed:

| Pre-1.0 | 1.0 |
|---|---|
| `CachedAsync::get_or_set_with` | `CachedAsync::async_get_or_set_with` |
| `CachedAsync::try_get_or_set_with` | `CachedAsync::async_try_get_or_set_with` |

```rust
// Before
let v = cache.get_or_set_with(k, || async { compute().await }).await;
// After
let v = cache.async_get_or_set_with(k, || async { compute().await }).await;
```

The old names were identical to the `Cached::get_or_set_with` /
`Cached::try_get_or_set_with` convenience aliases. Because the in-memory stores
implement **both** traits, any call site with both in scope (very common — e.g.
`use cached::*;`, or using sync `cache_get` next to an async get-or-set) failed
to compile with `error[E0034]: multiple applicable items in scope` and required
disambiguating UFCS. The `async_` prefix removes the collision and makes the
async path self-describing. Only direct trait users are affected — the
`#[cached]`/`#[once]` macros call the canonical `cache_*` methods and need no
changes.

---

## 8. ⚠️ Redis cache key format changed (data-impacting)

**This is the only change that affects data already stored in an external
system.** Read this section carefully if you use the Redis store.

The Redis cache key construction changed from raw concatenation to
colon-delimited joining with empty segments skipped:

| | Pre-1.0 | 1.0 |
|---|---|---|
| Format | `{namespace}{prefix}{key}` | `{namespace}:{prefix}:{key}` |
| Default-namespace example | `cached-redis-store:my_prefixmy_key` | `cached-redis-store:my_prefix:my_key` |

The default namespace (`cached-redis-store:`) has its trailing colon trimmed and
is then re-joined with `:`, so for default-namespace users the net effect is
that the prefix and key are now separated by a colon.

**Impact:** after upgrading, every lookup computes a *new* key that does not
match anything stored by the old version. Your Redis cache effectively
cold-starts — every entry is a miss until repopulated. Old entries are
orphaned and will be reclaimed by Redis when their original TTL expires.

**Your options:**

1. **Accept the cold start (recommended for most).** No action needed. The
   cache simply refills. Old keys expire on their own via their TTLs. This is
   safe as long as your backing function is idempotent and the temporary load
   from a cold cache is acceptable.
2. **Flush the old keys** if you want to reclaim memory immediately rather than
   waiting for TTL expiry (e.g. `redis-cli --scan --pattern 'cached-redis-store:*'`
   then delete — scope the pattern to your namespace/prefix).
3. **Preserve hits by matching the old layout.** If a cold start is
   unacceptable, choose a `namespace`/`prefix` whose colon-joined form
   reproduces your old concatenated keys. This is fiddly and generally not worth
   it compared to option 1 — prefer accepting the cold start unless you have a
   hard requirement.

If you run multiple instances, roll out the upgrade knowing that old and new
binaries will not share cache entries during the rollout window.

---

## 9. Behavioral changes (no API change required)

These do not require code edits but may change observable behavior:

- **`DiskCache::cache_get`** now returns a deserialization error for corrupted
  entries instead of silently treating them as a cache miss. If you previously
  relied on corruption being a miss, handle the error explicitly.
- **`DiskCache::cache_set`** now returns the raw previous value at the key
  (matching the `ConcurrentCached` contract and Redis behavior).
- **`DiskCache::remove_expired_entries`** now reports storage/deserialization
  errors instead of ignoring them.
- **`LruCache` / `LruTtlCache` / `ExpiringLruCache` `cache_reset`** now rebuilds
  the backing store instead of only clearing entries (releases capacity).
- **Timed `#[once]`** caches now start the TTL countdown *after* the function
  body finishes, not before it starts.
- **`LruTtlCache` validation errors** now use `std::io::ErrorKind::InvalidInput`
  instead of a raw OS error code; **`TtlSortedCache`** size-limit validation
  likewise uses `InvalidInput`.
- **Redis TTL handling** now rejects only zero durations, rounds sub-second
  non-zero TTLs up to one second, and avoids overflowing refresh expirations.
- **`ExpiringLruCache::cache_get`** removes expired entries on access instead of
  promoting them to most-recently-used (previously this could evict live
  entries ahead of expired ones).
- **Generic functions with `where` clauses** now work with `#[cached]`,
  `#[once]`, and `#[concurrent_cached]` (the macro-generated helper previously
  dropped the `where` clause). Note that `#[cached]`/`#[concurrent_cached]` back
  the cache with a `static`, so a generic parameter that would appear in the
  derived cache key/value type must be pinned to a concrete type via
  `key` + `convert` (and `ty` for `#[concurrent_cached]`); `#[once]` has no such
  constraint.
- **Async `DiskCache`** (`#[concurrent_cached(disk = true)]` on an `async fn`, or
  `DiskCache` used via `ConcurrentCachedAsync`) now runs the blocking `sled`
  operations on `tokio`'s blocking thread pool via `spawn_blocking` instead of
  blocking the async runtime. This requires a Tokio runtime context (already the
  case for `#[concurrent_cached]` async functions) and adds a
  `DiskCacheError::BackgroundTaskFailed`
  variant for the rare case where that blocking task is cancelled or panics. If
  you `match` on `DiskCacheError` exhaustively, add an arm for the new variant
  (or a `_ =>` catch-all) — otherwise the match fails to compile. Note: dropping
  an async `DiskCache` future does not cancel the in-flight `sled` operation
  (it completes on the blocking pool); this is safe (atomic `sled` ops) but a
  cancelled `cache_set`/`cache_remove` may still have taken effect on disk.

---

## 10. Feature-flag changes

- A new **`async_core`** feature provides the runtime-agnostic async traits.
  The **`async`** feature now enables `async_core` + `tokio` (Tokio-based sync
  primitives) and no longer pulls in `futures` / `async-trait`. If you depended
  on those transitively, add them explicitly to your own `Cargo.toml`.
- The timed in-memory stores (`TtlCache`, `LruTtlCache`, `TtlSortedCache`) and
  the `CacheTtl` trait are now behind the **`time_stores`** feature, which **is**
  in the default feature set. If you build with `default-features = false` and
  use any of these (e.g. the renamed `TimedCache`/`TimedSizedCache`/
  `ExpiringSizedCache`), add `time_stores` to your `cached` features or they will
  not resolve.
- The example files `basic_proc_macro` and `kitchen_sink_proc_macro` were
  renamed to `basic` and `kitchen_sink`.

---

## 11. New convenience APIs (additive)

These are not breaking changes, but are worth knowing about:

- **Ergonomic aliases on `Cached`**: `get`, `set`, `remove`, `contains`, `size`,
  `hits`, `misses`, `evictions`, `clear`, `reset` — short names that delegate to
  the `cache_*` methods. You can use either form; the aliases are zero-cost
  forwarding.
- **`CacheMetrics`**: `cache.metrics()` returns a `CacheMetrics` snapshot with
  `hits`, `misses`, `evictions`, `size`, `capacity` fields and a `hit_ratio()`
  helper.
- **Builder APIs**: every in-memory store now has `::builder()` with `on_evict`
  callback support and `try_build()` for fallible construction.
- **`CacheEvict` trait**: `evict()` sweeps expired entries from all timed/expiring
  stores and returns the count removed.
- **`CachedPeek` / `CachedRead`**: `CachedPeek::cache_peek` is a non-mutating
  lookup (no recency update, TTL refresh, or hit/miss metrics); `CachedRead`
  enables `#[cached(unsync_reads = true)]`, which serves cache hits under a
  shared read lock for stores whose reads don't mutate state.
- **`on_evict` callbacks**: pass `.on_evict(|k, v| { .. })` to any in-memory
  store builder to observe evictions (LRU/TTL/size).
- **`cache_delete` / `cache_try_set`**: `ConcurrentCached`/`ConcurrentCachedAsync::cache_delete`
  removes an entry without decoding/returning it (handy for corrupt entries);
  `Cached::cache_try_set` is the fallible insert used by stores like
  `TtlSortedCache` whose insertion can fail.
- **Macros at the crate root**: `#[cached]`, `#[once]`, and `#[concurrent_cached]`
  are now re-exported at the crate root, so `use cached::cached;` works in
  addition to `use cached::macros::cached;`.
- **Builder/error types at the crate root**: `DiskCacheBuilder`,
  `DiskCacheBuildError`, `RedisCacheBuilder`, `RedisCacheBuildError`, and
  `AsyncRedisCacheBuilder` are now re-exported at the crate root (previously only
  reachable as `cached::stores::…`), matching the in-memory `*Builder`
  re-exports. You can now name the error returned by `DiskCache`/`RedisCache`
  `build()` via the same path the cache type came from.

---

## 12. `IOCached`/`#[io_cached]` renamed; `InMemoryAdapter` removed

The "IO" naming was misleading: the trait's actual contract is a
self-synchronizing cache with a shared (`&self`) API and owned return values —
true for Redis/disk, but equally true for a future concurrent in-memory store.
The traits and the macro were renamed accordingly. **The contract and behavior
are unchanged — this is a pure rename.**

| Pre-1.0 | 1.0 |
|---|---|
| `IOCached` | `ConcurrentCached` |
| `IOCachedAsync` | `ConcurrentCachedAsync` |
| `#[io_cached(...)]` | `#[concurrent_cached(...)]` |
| `cached::macros::io_cached` | `cached::macros::concurrent_cached` |

```rust
// Before
use cached::macros::io_cached;
use cached::{IOCached, IOCachedAsync};

#[io_cached(redis = true, ttl = 30, map_error = r##"|e| MyErr(e)"##)]
fn lookup(k: u64) -> Result<String, MyErr> { /* ... */ }

impl IOCached<u64, String> for MyStore { /* ... */ }

// After
use cached::macros::concurrent_cached;
use cached::{ConcurrentCached, ConcurrentCachedAsync};

#[concurrent_cached(redis = true, ttl = 30, map_error = r##"|e| MyErr(e)"##)]
fn lookup(k: u64) -> Result<String, MyErr> { /* ... */ }

impl ConcurrentCached<u64, String> for MyStore { /* ... */ }
```

Rename whole identifiers only; replace `IOCachedAsync` before `IOCached` so the
shared substring isn't double-rewritten.

### `InMemoryAdapter` removed

`InMemoryAdapter<K, V, C>` is gone. It only wrapped a `Cached` store in a single
`parking_lot::Mutex` — for the macro path that is strictly worse than `#[cached]`
(which already generates the lock, with no `Result<_, Infallible>` indirection
or double-locking). Replacements:

- **Memoized function over an in-memory store:** use `#[cached]` / `#[once]`
  (with `ty` + `create` for a custom store) — never went through the adapter
  anyway.
- **A shareable in-memory cache object behind the concurrent trait:** implement
  `ConcurrentCached` directly on an `Arc<parking_lot::Mutex<YourStore>>`
  (~10 lines), or back the function with `RedisCache`/`DiskCache`.

> Default Redis prefix note: the auto-generated Redis key prefix token changed
> from `cached::macros::io_cached::<NAME>` to
> `cached::macros::concurrent_cached::<NAME>`. This only matters if you relied on
> the *default* prefix with a Redis store, and it is already subsumed by the
> Redis key-format cold-start in §8 — no extra action beyond §8.

> **`#[concurrent_cached(create = ...)]` + builder attrs now error.** When a
> `create` block is supplied the user fully constructs the store, so every
> store-builder attribute (`ttl`, `refresh`, `cache_prefix_block`, `disk_dir`,
> `connection_config`, `sync_to_disk_on_cache_change`) the macro would
> otherwise apply is now rejected with a unified diagnostic. Pre-1.0,
> `ttl`/`refresh` were rejected but the disk-builder attrs were silently
> ignored — `disk_dir = "/var/cache"` paired with `create` looked applied but
> wasn't. Fix: move the dropped settings into your `create` block, or remove
> them.

> **`#[concurrent_cached]` return-type check is structural.** Non-`Result`
> returns (`Option<T>`, `Vec<T>`, bare `T`, …) now fail at attribute
> expansion with a clear spanned message instead of inside the generated body.
> A `Result` *type alias* renamed away from `Result` (e.g.
> `type MyResult<T> = Result<T, E>; -> MyResult<u32>`) is not recognized — the
> macro only sees tokens, the same limitation already documented for
> `with_cached_flag`/`Return`. Use a literal `Result<…, …>` return type.

---

## Quick migration checklist

- [ ] Rename store types (§1) and their modules; update constructors and
      refresh/`store()` accessors.
- [ ] `CanExpire` → `Expires`, including all trait bounds (§2).
- [ ] Replace declarative `cached!`/`cached_key!`/… macros with `#[cached]` /
      `#[once]` / `#[concurrent_cached]` (§3).
- [ ] `use cached::proc_macro::*` → `use cached::macros::*` (§4).
- [ ] Macro attrs: `time` → `ttl`, `time_refresh` → `refresh` (§5).
- [ ] Builder methods: drop `set_` prefix; `set_lifespan` → `ttl` (§6).
- [ ] TTL methods: `cache_ttl`/`cache_set_ttl`/`cache_unset_ttl` removed from
      `Cached`; use `CacheTtl::ttl`/`set_ttl`/`unset_ttl` on timed stores.
      `cache_set_refresh` → `set_refresh_on_hit` (§7).
- [ ] **Decide your Redis cold-start strategy** (§8).
- [ ] Review behavioral changes for code that depended on the old behavior (§9).
- [ ] Adjust feature flags if you relied on `futures`/`async-trait` transitively
      (§10).
- [ ] `IOCached`/`IOCachedAsync` → `ConcurrentCached`/`ConcurrentCachedAsync`;
      `#[io_cached]` → `#[concurrent_cached]`; drop any `InMemoryAdapter` use (§12).

After updating, `cargo build` will surface most remaining issues — the macros
emit targeted compile errors for the renamed `time`/`time_refresh` attributes.
