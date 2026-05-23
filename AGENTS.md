# cached — AI Agent Instructions

## Contributing Guidelines
Before making any changes, read and follow **[CONTRIBUTING.md](CONTRIBUTING.md)**.
Key points:
- Run `make fmt` before committing
- Update `CHANGELOG.md` with a description of what changed and why
- After editing `src/lib.rs`, sync the README with `make docs` and verify with `make check/readme`
- After adding or removing Makefile targets, update `make help` and verify with `make check/help`
- Run `make ci` to validate the full pipeline before submitting

---

## Temp Files
Write any scratch files, research dumps, or intermediate agent outputs to `local/` — it is gitignored and always safe to write to. Do not create temp files elsewhere in the repo.

---

## Project Overview
`cached` is a Rust crate providing generic cache implementations and simplified function memoization. Workspace members:
- `cached` — main crate (`src/`)
- `cached_proc_macro` — procedural macro crate (`cached_proc_macro/src/`)
- `cached_proc_macro_types` — shared types used by the proc macro (`cached_proc_macro_types/src/lib.rs`); currently just `Return<T>`
- `examples/wasm` — WASM example (separate Cargo workspace member)

---

## Store Types (current names as of v1.1)

| Type | Module | Description |
|---|---|---|
| `UnboundCache<K,V>` | `cached::stores` | Unbounded HashMap-backed cache |
| `LruCache<K,V>` | `cached::stores` | LRU eviction, size-bounded |
| `TtlCache<K,V>` | `cached::stores` | Global TTL, no size limit; requires `time_stores` |
| `LruTtlCache<K,V>` | `cached::stores` | LRU + global TTL, size-bounded; requires `time_stores` |
| `TtlSortedCache<K,V>` | `cached::stores` | TTL-ordered, optional size limit; requires `time_stores` |
| `ExpiringLruCache<K,V>` | `cached::stores` | LRU, size-bounded, per-value expiry via `Expires` trait |
| `ExpiringCache<K,V>` | `cached::stores` | Unbounded HashMap-backed, per-value expiry via `Expires` trait; default store for `#[cached(expires = true)]` |

**`ExpiringCache` / `ExpiringLruCache` notes:**
- Neither store requires the `time_stores` feature — they are always available.
- `ExpiringLruCache::store()` is `pub` (released in v1.0.0 — cannot be changed to `pub(crate)` without a breaking change). `ExpiringCache` has no public `store()` accessor.
- `cache_get` and `cache_get_mut` on `ExpiringCache` use two hash lookups on the hit path due to stable Rust's NLL borrow-checker limitation: the first lookup checks expiry (dropping the reference via `.map`), the second returns the value reference or calls `remove_entry`. Polonius (nightly) would allow a single lookup. This is intentional and documented in the source with a comment.
- `cache_get_with_expiry_status` (from `CloneCached`) intentionally **leaves an expired entry in the map** so `result_fallback` can clone and return it as a stale-but-present value on `Err`. The stale entry remains visible via `cache_size()` and `CachedIter` until the next `cache_get`, `evict()`, or explicit `cache_remove`. When `result_fallback = true` and `expires = true`, callers receive `Ok(stale_value)` where `stale_value.is_expired() == true` — callers must check the value's expiry themselves if they need to distinguish a fresh result from a stale fallback.
- `CachedIter::iter()` filters expired entries from the iterator but does **not** remove them from the map. Call `evict()` periodically for high-cardinality workloads.

**Renamed from pre-1.0** (do not use old names — they no longer exist):
- `SizedCache` → `LruCache`
- `TimedCache` → `TtlCache`
- `TimedSizedCache` → `LruTtlCache`
- `ExpiringSizedCache` → `TtlSortedCache`
- `ExpiringValueCache` → `ExpiringLruCache`

**Builder APIs**: All stores expose a `::builder()` constructor (e.g., `LruCache::builder()`, `TtlCache::builder()`). Builders support an `on_evict(|k, v| { ... })` callback fired on every evicted entry, and `try_build()` for fallible construction.

**`TimedEntry<V>`**: Exposed from `TtlCache::store()` and `LruTtlCache::store()` for direct introspection; fields `instant: Instant` and `value: V`.

---

## Key Traits

| Trait | Purpose |
|---|---|
| `Cached<K,V>` | Core cache operations: get, set, remove, clear, metrics |
| `CachedAsync<K,V>` | Async `async_get_or_set_with` / `async_try_get_or_set_with` |
| `CachedRead<K,V>` | Shared-ref reads (no mutation); enables `unsync_reads` |
| `CachedPeek<K,V>` | Non-mutating peek; skips recency/TTL refresh and metrics |
| `CachedIter<K,V>` | Iteration over cache entries |
| `CloneCached<K,V>` | `cache_get_with_expiry_status` for timed caches returning owned values |
| `CacheEvict` | `evict() -> usize` to sweep expired entries; fires `on_evict` |
| `Expires` | Implemented by values in `ExpiringLruCache`; provides `is_expired()` |
| `ConcurrentCached<K,V>` | Self-synchronizing cache with a shared `&self` API (Redis, Disk) |
| `ConcurrentCachedAsync<K,V>` | Async self-synchronizing cache |
| `CacheTtl` | `ttl()` / `set_ttl()` / `unset_ttl()` on timed stores |

**`CacheMetrics`**: Snapshot struct returned by `cache.metrics()` on any `Cached` store. Fields: `hits`, `misses`, `evictions` (all `Option<u64>`), `size: usize`, `capacity: Option<usize>`. Has a `hit_ratio() -> Option<f64>` method.

---

## Proc Macro Module
The proc macros are re-exported at `cached::macros` (feature `proc_macro`, on by default).

```rust
use cached::macros::cached;
use cached::macros::once;
use cached::macros::concurrent_cached;
```

**Renamed from pre-1.0**: was `cached::proc_macro`. The Cargo feature flag is still named `proc_macro`.

The macro attributes use `ttl =` (not `time =`) and `refresh =` (not `time_refresh =`). Note: `#[once]` supports `ttl =` but has never had a `refresh =` attribute (single-value cache, refresh-on-hit is not applicable).

**Additional `#[cached]` / `#[once]` attributes** (beyond `name`, `size`, `ttl`, `refresh`, `ty`, `create`, `key`, `convert`, `result`, `option`, `with_cached_flag`):
- `sync_writes`: `false` (default), `true` / `"default"` (whole-cache lock), or `"by_key"` (per-key bucketed locks; `#[cached]` only)
- `sync_writes_buckets`: `usize` — number of per-key lock buckets for `sync_writes = "by_key"`; defaults to 64
- `sync_lock`: `"rwlock"` (default) or `"mutex"` — the lock type wrapping the generated cache static
- `unsync_reads`: `bool` — use a shared read lock for cache hits; only works for stores implementing `CachedRead` (e.g. `UnboundCache`, `TtlSortedCache`, `HashMap`)
- `result_fallback`: `bool` — on `Err`, return the last cached `Ok` value instead; requires `result = true` and a `CloneCached` store

**`_prime_cache` helpers**: Every macro-generated function `foo(…)` also emits `foo_prime_cache(…)` for manually refreshing cached entries (bypasses the cache and forces re-execution). `#[once]` functions emit `foo_prime_cache()` with no arguments.

**Generics**: generic functions with `where` clauses are supported. The macros clone the original `syn::Signature` (preserving the `where` clause, lifetimes, const generics) for the generated origin/`inner` helper — quoting `#generics` alone would drop the `where` clause. Because `#[cached]`/`#[concurrent_cached]` store the cache in a `static`, a generic parameter that would land in the derived key/value type must be pinned via `key` + `convert` (and `ty`); `#[once]`'s static only holds the concrete value type, so it is unconstrained.

---

## Build
```bash
cargo build --all-features
```

---

## Format
```bash
cargo fmt
```
Check only (no writes):
```bash
cargo fmt --check
```

---

## Lint
```bash
cargo clippy --all-features --all-targets --examples --tests
```

---

## Test
Tests need a Redis instance on port 6399. Start it first:
```bash
docker run --rm --name cached-tests -p 6399:6379 -d redis
```
Run tests:
```bash
CACHED_REDIS_CONNECTION_STRING=redis://127.0.0.1:6399 \
  cargo test --all-features -- --nocapture
```
Or use the Makefile (auto-starts Docker):
```bash
make tests
```

### Trybuild golden files
The `tests/ui/` directory contains compile-fail tests with `.stderr` golden files. When type names or error messages change, regenerate them with:
```bash
TRYBUILD=overwrite cargo test --features "proc_macro,time_stores"
```

---

## Full CI Check
```bash
make ci
```
This runs: `make check` (fmt + clippy + readme) -> `make tests` -> `make examples`.

---

## README Sync
`README.md` is auto-generated from `src/lib.rs` doc comments — **never edit README.md directly**.
```bash
make docs          # regenerate
make check/readme  # verify in sync
```

---

## Mandatory Verification After Every Change
After making **any** code change, run these steps in order:
1. **Format** — `cargo fmt`
2. **Lint** — `cargo clippy --all-features --all-targets --examples --tests`
3. **Test** — `make tests` (or `cargo test --all-features` with Redis running)
4. If `src/lib.rs` changed — `make docs && make check/readme`

Do **not** present a change as complete until all verification steps pass.

---

## Key Cargo Features

| Feature | Description |
|---|---|
| `proc_macro` (default) | `#[cached]`, `#[once]`, `#[concurrent_cached]` macros |
| `ahash` (default) | ahash hasher for internal hash maps |
| `time_stores` (default) | `TtlCache`, `LruTtlCache`, `TtlSortedCache` |
| `async_core` | Async support marker (no runtime); use with custom async runtimes |
| `async` | Async support via Tokio (enables `async_core` + `tokio`) |
| `async_tokio_rt_multi_thread` | Tokio multi-thread runtime (required for `#[tokio::test]`) |
| `redis_store` | Synchronous Redis backend |
| `redis_tokio` | Async Redis backend (Tokio) |
| `redis_smol` | Async Redis backend (smol); implies `redis_store` + `async` |
| `redis_connection_manager` | Redis connection-manager support |
| `redis_async_cache` | Redis client-side caching over RESP3 for async caches |
| `disk_store` | Disk-backed cache via `sled` |
| `wasm` | WASM compatibility |

---

## Important Paths

| Path | Purpose |
|---|---|
| `src/lib.rs` | Main library entry point + doc comments (source of truth for README) |
| `src/stores/` | Cache store implementations |
| `src/macros.rs` | Proc macro re-export module (`cached::macros`) |
| `src/stores/mod.rs` | Store re-exports, `CacheEvict` impls, `BuildError` type |
| `cached_proc_macro/src/cached.rs` | `#[cached]` and `#[once]` macro implementation |
| `cached_proc_macro/src/once.rs` | `#[once]`-specific macro implementation |
| `cached_proc_macro/src/concurrent_cached.rs` | `#[concurrent_cached]` macro implementation |
| `cached_proc_macro/src/helpers.rs` | Shared proc macro utilities |
| `cached_proc_macro_types/src/lib.rs` | `Return<T>` type for `with_cached_flag` |
| `tests/cached.rs` | Integration tests |
| `tests/ui/` | Compile-fail trybuild tests + `.stderr` golden files |
| `examples/` | Runnable usage examples |
| `docs/MIGRATION-1.0.md` | Human-readable 0.x → 1.0 migration guide |
| `docs/MIGRATION-1.0-AGENT.md` | Machine-oriented 0.x → 1.0 migration rules for automated tooling |
| `local/` | Gitignored scratch space — use for any temp/intermediate files |
| `Makefile` | All build/test/lint/example targets |
