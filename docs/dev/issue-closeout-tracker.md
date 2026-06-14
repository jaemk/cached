# Issue close-out tracker

Source of truth for the close-out pass after `260609.next-major-batch` lands.
**Do not post or close anything until that branch is merged.**

Generated from the approved plan at `.claude/plans/joyful-tickling-bee.md`.

---

## Bucket 1: close-now

These issues are already resolved by work that landed before this batch (1.0/2.0/redb/existing
features). Close immediately after this tracker is reviewed; no batch landing required.

| # | Title | Reporter | Resolving feature |
|---|-------|----------|-------------------|
| [#257](https://github.com/jaemk/cached/issues/257) | Interest in feature disabling use of expiring_cache / std::time? | @randombit | `time_stores` feature flag |
| [#248](https://github.com/jaemk/cached/issues/248) | Consider using `std::sync::LazyLock` instead of `once_cell::sync::Lazy` | @lbeschastny | `std::sync::LazyLock` already used |
| [#246](https://github.com/jaemk/cached/issues/246) | Dynamic entry ttl. | @ruseinov | `expires=true` / `Expires` trait |
| [#238](https://github.com/jaemk/cached/issues/238) | Question: Way to specify alternative lifetime for new entry in cached method? | @hcldan | `expires=true` / `Expires` trait |
| [#229](https://github.com/jaemk/cached/issues/229) | io_cached doesn't support sync_writes | @mharkins-cosm | `#[cached(sync_writes = "by_key")]` (concurrent stores synchronize internally) |
| [#219](https://github.com/jaemk/cached/issues/219) | `io_cached` doesn't have `result` and `option` flags | @omid | native `Result` / `Option` handling on `concurrent_cached` |
| [#217](https://github.com/jaemk/cached/issues/217) | Cached proc macro doesn't keep where clause | @gerald-pinder-omnicell | where clauses preserved |
| [#215](https://github.com/jaemk/cached/issues/215) | cached crate with proc macros broken | @demoray | resolved in 2.x |
| [#209](https://github.com/jaemk/cached/issues/209) | Cache hit / miss rate metrics when using `cached` procedural macro | @kembofly | `cache_hits` / `cache_misses` fields |
| [#206](https://github.com/jaemk/cached/issues/206) | DiskCache blobs aren't cleaned on overwrite | @RustyNova016 | redb DiskCache (no blob leaks) |
| [#197](https://github.com/jaemk/cached/issues/197) | Cache clear operation | @9999years | `cache_clear` on `Cached` trait |
| [#187](https://github.com/jaemk/cached/issues/187) | Async disk cache | @bschreck | `AsyncRedbCache` |
| [#184](https://github.com/jaemk/cached/issues/184) | 2021 edition? | @jqnatividad | Rust 2021 edition |
| [#164](https://github.com/jaemk/cached/issues/164) | Retrieve cache expiration time from cached function result | @inferrna | `expires=true` / `Expires` trait |
| [#158](https://github.com/jaemk/cached/issues/158) | sync_writes isn't working correctly when different values for function parameters are used | @0xForerunner | `sync_writes="by_key"` |
| [#144](https://github.com/jaemk/cached/issues/144) | Consider only changing patch versions when making non breaking releases | @samanpa | semver followed since 1.0 |
| [#142](https://github.com/jaemk/cached/issues/142) | Mio & Tokio causing wasm build to fail | @samdenty | optional async features |
| [#141](https://github.com/jaemk/cached/issues/141) | Cron-like cache clearing | @arxdeus | `time_stores` + manual invalidation |
| [#136](https://github.com/jaemk/cached/issues/136) | Errors seen while using AsyncRedisCache | @rajesh-blueshift | `std::sync::LazyLock` (no once_cell dep) |
| [#135](https://github.com/jaemk/cached/issues/135) | Feature Request: Ignore certain function arguments | @phayes | `convert` attribute |
| [#134](https://github.com/jaemk/cached/issues/134) | `#![feature]` may not be used on the stable release channel from thiserror | @inferrna | thiserror updated / stable |
| [#119](https://github.com/jaemk/cached/issues/119) | associated `static` items are not allowed | @gitmalong | resolved in 2.x |
| [#115](https://github.com/jaemk/cached/issues/115) | Feature Request: A cache where the value knows how to determine whether it is expired | @absoludity | `expires=true` / `Expires` trait |
| [#113](https://github.com/jaemk/cached/issues/113) | Behavior when function returns a `Result<T, E>`? | @phayes | `result=true` attribute |
| [#111](https://github.com/jaemk/cached/issues/111) | `cached` compile error "expected struct / found reference" | @bkontur | resolved in 2.x |
| [#99](https://github.com/jaemk/cached/issues/99) | Cache not refreshed. | @lz1998 | `refresh` attribute |
| [#96](https://github.com/jaemk/cached/issues/96) | Compile error if function args are `mut` | @jherman3 | `mut` args supported |
| [#92](https://github.com/jaemk/cached/issues/92) | Feature request: soft and hard timeouts for Result/Option | @axos88 | `result_fallback` attribute |
| [#83](https://github.com/jaemk/cached/issues/83) | Would using RwLock instead of Mutex make sense? | @adambezecny | `sync_writes="by_key"` uses key-level locks |
| [#62](https://github.com/jaemk/cached/issues/62) | Could there be a "locked" version? | @bbigras | `sync_writes="by_key"` |
| [#58](https://github.com/jaemk/cached/issues/58) | Add more documentation around proc macros | @jaemk | extensive proc macro docs added |
| [#49](https://github.com/jaemk/cached/issues/49) | Need to serialize/deserialize the cache | @Stargateur | `DiskCache` / `RedisCache` (serde) |
| [#48](https://github.com/jaemk/cached/issues/48) | Lifetimes not added to inner fn in proc macro | @coadler | resolved in 2.x |
| [#43](https://github.com/jaemk/cached/issues/43) | make a copy of macro tests for the proc macro | @jaemk | proc macro tests exist |
| [#38](https://github.com/jaemk/cached/issues/38) | Naming on Cached trait | @Stargateur | unprefixed `cache_get`/`cache_set`/etc. aliases |
| [#254](https://github.com/jaemk/cached/issues/254) | Question: using this crate in a library I'm building for C | @hcldan | support answer |
| [#163](https://github.com/jaemk/cached/issues/163) | Any plan on Set collection? | @lzy1g1225 | stale / out of scope |

### Drafted replies

---

**#257** — @randombit

```
Hi @randombit,

This is covered by the `time_stores` feature flag (opt-in since 2.0). Omitting it removes
the `std::time` dependency entirely. Closing.
```

---

**#248** — @lbeschastny

```
Hi @lbeschastny,

Done. The macros now emit `std::sync::LazyLock` instead of `once_cell::sync::Lazy` — the
`once_cell` dependency was dropped entirely in 2.0. Closing.
```

---

**#246** — @ruseinov

```
Hi @ruseinov,

This is supported via the `expires=true` macro attribute combined with the `Expires` trait.
Each function call can return a custom TTL by implementing `Expires` on the return type, or
by wrapping the value in `ExpiresValue`. See the `expires_per_key` example. Closing.
```

---

**#238** — @hcldan

```
Hi @hcldan,

Per-entry lifetimes are supported via the `expires=true` attribute and the `Expires` trait
(or `ExpiresValue` wrapper). The TTL is determined from the returned value on each call.
Closing.
```

---

**#229** — @mharkins-cosm

```
Hi @mharkins-cosm,

`io_cached` was replaced by `concurrent_cached` in 2.x, which intentionally does not take
`sync_writes` (the backing store synchronizes internally; uncached calls are not deduplicated).
Per-key deduplication of concurrent first calls is available on the in-memory path via
`#[cached(sync_writes = "by_key")]`. Closing.
```

---

**#219** — @omid

```
Hi @omid,

`io_cached` was replaced by `concurrent_cached` in 2.x. `Result` handling is built in (`Ok` is
cached, `Err` is not; `cache_err = true` caches failures), and `Option` returns are supported on
the in-memory sharded path (`None` not cached; `cache_none = true` to cache it). Closing.
```

---

**#217** — @gerald-pinder-omnicell

```
Hi @gerald-pinder-omnicell,

Where clauses are preserved on the generated inner function. Closing.
```

---

**#215** — @demoray

```
Hi @demoray,

This was resolved in the 2.x rewrite. Closing.
```

---

**#209** — @kembofly

```
Hi @kembofly,

The generated static cache has `cache_hits` and `cache_misses` fields you can read directly,
e.g. `MY_FN.read().cache_hits`. Closing.
```

---

**#206** — @RustyNova016

```
Hi @RustyNova016,

The sled-backed `DiskCache` has been replaced with a redb backend. redb's transactional
writes mean overwrites replace the entry atomically with no orphaned blobs. Closing.
```

---

**#197** — @9999years

```
Hi @9999years,

`cache_clear` is a required method on the `Cached` trait and is implemented by all store
types. Closing.
```

---

**#187** — @bschreck

```
Hi @bschreck,

An async disk cache (`AsyncRedbCache`) backed by redb is now available. Closing.
```

---

**#184** — @jqnatividad

```
Hi @jqnatividad,

The crate has used the Rust 2021 edition since 1.0. Closing.
```

---

**#164** — @inferrna

```
Hi @inferrna,

The `expires=true` attribute plus the `Expires` trait lets each function call return a
custom TTL alongside the cached value. See the `expires_per_key` example. Closing.
```

---

**#158** — @0xForerunner

```
Hi @0xForerunner,

This is fixed by `sync_writes="by_key"`, which uses a per-key lock so concurrent calls
with different arguments proceed independently. Closing.
```

---

**#144** — @samanpa

```
Hi @samanpa,

The crate has followed semver strictly since 1.0 — patch releases for bug fixes, minor for
additive changes, major for breaking changes. Closing.
```

---

**#142** — @samdenty

```
Hi @samdenty,

Async dependencies (tokio, smol) are behind optional feature flags and are not pulled in
by default. Closing.
```

---

**#141** — @arxdeus

```
Hi @arxdeus,

Time-based caches are available via the `time_stores` feature (`TtlCache`,
`LruTtlCache`, etc.). Manual invalidation is possible via `cache_remove` or
`cache_clear` on the static cache handle. Closing.
```

---

**#136** — @rajesh-blueshift

```
Hi @rajesh-blueshift,

The `once_cell` dependency was removed in 2.0; the macros now use `std::sync::LazyLock`
from stable Rust. This should resolve the build errors you saw. Closing.
```

---

**#135** — @phayes

```
Hi @phayes,

The `convert` attribute lets you ignore arguments that shouldn't be part of the cache key.
For example: `#[cached(convert = "{ arg_to_use.clone() }")]`. Closing.
```

---

**#134** — @inferrna

```
Hi @inferrna,

The `thiserror` dependency was updated and no nightly features are required. Closing.
```

---

**#119** — @gitmalong

```
Hi @gitmalong,

Associated statics in impl blocks are no longer generated; the macro uses module-level
statics. This was resolved in the 2.x rewrite. Closing.
```

---

**#115** — @absoludity

```
Hi @absoludity,

The `expires=true` attribute combined with the `Expires` trait does exactly this: the
cached value itself determines its own expiry. Closing.
```

---

**#113** — @phayes

```
Hi @phayes,

`result=true` unwraps `Result<T, E>` — only `Ok` values are cached, `Err` values pass
through without caching. `option=true` does the same for `Option<T>`. Closing.
```

---

**#111** — @bkontur

```
Hi @bkontur,

Reference inputs are handled correctly in current versions. Closing.
```

---

**#99** — @lz1998

```
Hi @lz1998,

The `refresh = true` attribute resets the TTL on each cache hit, keeping hot entries
alive. Closing.
```

---

**#96** — @jherman3

```
Hi @jherman3,

`mut` function arguments are supported without issue. Closing.
```

---

**#92** — @axos88

```
Hi @axos88,

The `result_fallback=true` attribute supports this pattern: when the function returns `Err`,
the macro returns the previously-cached value (if any) rather than the error. Closing.
```

---

**#83** — @adambezecny

```
Hi @adambezecny,

`sync_writes="by_key"` uses a per-key lock (DashMap-based sharded mutex), so concurrent
calls with different keys don't block each other. Closing.
```

---

**#62** — @bbigras

```
Hi @bbigras,

`sync_writes="by_key"` serializes concurrent calls for the same key so only one fetch runs
and the rest wait for the cached result. Closing.
```

---

**#58** — @jaemk

```
Hi @jaemk,

Extensive proc macro documentation is now in the crate root (`src/lib.rs`) and the README,
covering all attributes with examples. Closing.
```

---

**#49** — @Stargateur

```
Hi @Stargateur,

`DiskCache` (backed by redb) and `RedisCache` both serialize/deserialize via serde, enabling
persistent caches that survive process restarts. Closing.
```

---

**#48** — @coadler

```
Hi @coadler,

Lifetimes on function signatures are passed through to the inner function correctly.
Closing.
```

---

**#43** — @jaemk

```
Hi @jaemk,

Proc macro tests are in `tests/cached.rs` and `tests/` alongside UI / compile-fail tests.
Closing.
```

---

**#38** — @Stargateur

```
Hi @Stargateur,

The `Cached` trait now exposes unprefixed aliases (`cache_get`, `cache_set`, `cache_remove`,
`cache_clear`, `cache_size`) alongside the original names. Closing.
```

---

**#254** — @hcldan (support answer)

```
Hi @hcldan,

The `cached` crate is a pure Rust library and doesn't expose a C API. For FFI use you
would need to write a thin `extern "C"` wrapper crate on top of it yourself. Closing.
```

---

**#163** — @lzy1g1225 (stale)

```
Hi @lzy1g1225,

There are no plans to add a Set collection type to this crate. Closing as out of scope.
```

---

## Bucket 2: close-when-this-batch-lands

Close these after `260609.next-major-batch` (the next major release) is merged and published.
Reply text says "fixed in the upcoming major release."

| # | Title | Reporter | Resolved by |
|---|-------|----------|-------------|
| [#230](https://github.com/jaemk/cached/issues/230) | `key` argument name collision | @publicqi | binding hygiene (`__cached_` prefix) |
| [#114](https://github.com/jaemk/cached/issues/114) | Cannot use key as a function argument | @paulvt | binding hygiene (`__cached_` prefix) |
| [#202](https://github.com/jaemk/cached/issues/202) | proc_macro: support args which are `&T` and `Option<&T>` | @BaxHugh | reference inputs |
| [#203](https://github.com/jaemk/cached/issues/203) | Support `&T` and `Option<&T>` in input | @BaxHugh | reference inputs |
| [#157](https://github.com/jaemk/cached/issues/157) | Allow reexport | @inferrna | re-export hygiene (`proc-macro-crate`) |
| [#149](https://github.com/jaemk/cached/issues/149) | Feature Request: Floating-Point ttl | @waterlubber | `ttl_millis` attribute |
| [#146](https://github.com/jaemk/cached/issues/146) | Feature Request: bool argument that forces a cache refresh | @0xForerunner | `force_refresh` attribute |
| [#16](https://github.com/jaemk/cached/issues/16) | macro: Not working inside impl blocks | @behnam | `in_impl=true` attribute |
| [#140](https://github.com/jaemk/cached/issues/140) | Feature request: Skip self field to allow caching methods | @Serock3 | `in_impl=true` attribute |
| [#179](https://github.com/jaemk/cached/issues/179) | Unnecessary `&mut V` with `get_or_set_with` | @hanako-eo | `get_or_set_with` returns `&V` |
| [#196](https://github.com/jaemk/cached/issues/196) | Borrowed keys and values for IOCached::set_cache | @9999years | `SerializeCached` / `cache_set_ref` |
| [#195](https://github.com/jaemk/cached/issues/195) | Borrowed keys and values for `IOCached::set_cache` | @9999years | `SerializeCached` / `cache_set_ref` |
| [#231](https://github.com/jaemk/cached/issues/231) | Rustls support for Redis | @rbozan | `redis_tokio_rustls` / `redis_smol_rustls` features |
| [#200](https://github.com/jaemk/cached/issues/200) | Add `cache_clear` operation | @9999years | `cache_clear` on Redis stores |
| [#180](https://github.com/jaemk/cached/issues/180) | Ability to configure a SizedCache `size` based on runtime data | @hcldan | `LruCache::set_max_size` |
| [#260](https://github.com/jaemk/cached/issues/260) | Debian: cargo test --no-default-features | @kpcyrd | `no_run` doctest fix |
| [#78](https://github.com/jaemk/cached/issues/78) | Document how this should work on floats? | @EvanCarroll | README float/convert docs |
| [#21](https://github.com/jaemk/cached/issues/21) | Examples: cache invalidation | @flavius | `basic.rs` invalidation example |
| [#236](https://github.com/jaemk/cached/issues/236) | Add example of passing dyn trait instance | @breadrock1 | `struct_method.rs` dyn/free-fn example |
| [#80](https://github.com/jaemk/cached/issues/80) | How to approach generics? | @big-lip-bob | generic-fn error + README workaround |
| [#245](https://github.com/jaemk/cached/issues/245) | Consider creating git tags and eventually GitHub releases | @flavio | automated release tagging |
| [#237](https://github.com/jaemk/cached/issues/237) | Replace sled crate? | @jqnatividad | redb DiskCache (sled removed) |
| [#206](https://github.com/jaemk/cached/issues/206) | DiskCache blobs aren't cleaned on overwrite | @RustyNova016 | redb DiskCache (also in close-now; lands with major) |
| [#20](https://github.com/jaemk/cached/issues/20) | Add ability to store cache to disk | @cjbassi | redb `DiskCache` / `AsyncRedbCache` |

> Note: #206 appears in both close-now (already resolved by the redb backend that is staged
> on master) and close-when-batch-lands per the plan's "also close when the major ships (redb)"
> note. It can be closed as part of either pass; once is sufficient.

### Drafted replies

---

**#230** — @publicqi

```
Hi @publicqi,

Fixed in the upcoming major release. The macro now uses `__cached_`-prefixed internal
bindings, so user argument names like `key`, `result`, `cache`, and `lock` no longer
collide with macro-generated variables.
```

---

**#114** — @paulvt

```
Hi @paulvt,

Fixed in the upcoming major release. Internal macro bindings are now prefixed with
`__cached_`, so a function argument named `key` (or `result`, `cache`, `lock`, etc.) no
longer shadows or collides with them.
```

---

**#202** — @BaxHugh

```
Hi @BaxHugh,

Fixed in the upcoming major release. `&T` arguments are now automatically owned for the
cache key (via `.to_owned()`), and `Option<&T>` is mapped with `.map(|v| (*v).to_owned())`.
No `convert` workaround needed.
```

---

**#203** — @BaxHugh

```
Hi @BaxHugh,

Fixed in the upcoming major release. `&T` and `Option<&T>` inputs are now handled
automatically in the default key derivation path. See #202 for the linked fix.
```

---

**#157** — @inferrna

```
Hi @inferrna,

Fixed in the upcoming major release. The proc macro crate now uses `proc-macro-crate` to
resolve its own crate path at expansion time, so it works correctly when re-exported under
a different name.
```

---

**#149** — @waterlubber

```
Hi @waterlubber,

Fixed in the upcoming major release via the new `ttl_millis` attribute. You can now write
`#[cached(ttl_millis = 500)]` for sub-second TTLs without floating-point. `ttl` (seconds)
and `ttl_millis` are mutually exclusive.
```

---

**#146** — @0xForerunner

```
Hi @0xForerunner,

Added in the upcoming major release via the `force_refresh` attribute. Pass a boolean
expression block (curly braces, like `convert`) over the function arguments. For a dedicated
flag, exclude it from the key so forced and normal calls share one entry:

    #[cached(key = "u32", convert = "{ id }", force_refresh = "{ bypass }")]
    fn fetch(id: u32, bypass: bool) -> Data { ... }

When `bypass` is true the cache is skipped and the fresh result overwrites the stored entry.
```

---

**#16** — @behnam

```
Hi @behnam,

Fixed in the upcoming major release via the new `in_impl = true` attribute:

    impl MyStruct {
        #[cached(in_impl = true)]
        fn compute(&self, key: i32) -> i32 { ... }
    }

This moves the cache static inside the function body so multiple methods can share a name
without collision.
```

---

**#140** — @Serock3

```
Hi @Serock3,

Added in the upcoming major release via `in_impl = true`. See #16 for details.
```

---

**#179** — @hanako-eo

```
Hi @hanako-eo,

Fixed in the upcoming major release. `cache_get_or_set_with` and `cache_try_get_or_set_with`
(and their async counterparts) now return `&V` / `Result<&V, E>`. The previous `&mut V`
behavior is preserved in new `*_mut` variants (`cache_get_or_set_with_mut`, etc.).
```

---

**#196** — @9999years

```
Hi @9999years,

Added in the upcoming major release. The new `SerializeCached` trait exposes
`cache_set_ref(&K, &V)` for stores that serialize internally (redb, Redis), avoiding the
clone that `cache_set(K, V)` requires. `SerializeCachedAsync` covers the async case.
```

---

**#195** — @9999years

```
Hi @9999years,

This is addressed by #196. The new `SerializeCached` / `SerializeCachedAsync` traits
(added in the upcoming major release) accept borrowed keys and values directly. Closing
as a duplicate of #196.
```

---

**#231** — @rbozan

```
Hi @rbozan,

Added in the upcoming major release. New Cargo features `redis_tokio_rustls`,
`redis_tokio_native_tls`, `redis_smol_rustls`, and `redis_smol_native_tls` let you pick
your TLS backend explicitly. The base `redis_tokio` / `redis_smol` features are now
TLS-agnostic.
```

---

**#200** — @9999years

```
Hi @9999years,

Added in the upcoming major release. Both `RedisCache` and `AsyncRedisCache` now implement
`cache_clear` / `async_cache_clear`, which scan the cache's namespace prefix and delete all
matching keys. Note that this is O(n) and scoped to the cache's namespace, not a server flush.
```

---

**#180** — @hcldan

```
Hi @hcldan,

Added in the upcoming major release. `LruCache` now has `set_max_size(&mut self, usize)`
(and `try_set_max_size`) to resize at runtime. If the new capacity is smaller than the
current size, the least-recently-used entries are evicted immediately. You can pass the
initial size via the `create` attribute for startup sizing:

    #[cached(create = "{ LruCache::with_size(load_config().cache_size) }")]
```

---

**#260** — @kpcyrd

```
Hi @kpcyrd,

Fixed in the upcoming major release. The problematic doctest in `src/lib.rs` is now marked
`no_run` (or gated appropriately), so `cargo test --no-default-features` completes without
error. This unblocks Debian package builds.
```

---

**#78** — @EvanCarroll

```
Hi @EvanCarroll,

Documented in the upcoming major release. The README now calls out floats and structs
containing floats as the canonical case for `convert`, with a short example using
`OrderedFloat` or `format!` to derive a hashable key.
```

---

**#21** — @flavius

```
Hi @flavius,

Added in the upcoming major release. `examples/basic.rs` now includes an `invalidate_*`
function showing `cache_remove` to evict a single entry and demonstrating that the next
call recomputes.
```

---

**#236** — @breadrock1

```
Hi @breadrock1,

Added in the upcoming major release. `examples/struct_method.rs` now shows the canonical
workaround: extract the logic into a free `#[cached]` function and call it from the method.
A `dyn Trait` variant keyed on an object ID is also included.
```

---

**#80** — @big-lip-bob

```
Hi @big-lip-bob,

Addressed in the upcoming major release. Generic functions now produce a clear compile
error from the macro, and the README documents the monomorphic-wrapper pattern as the
recommended workaround:

    fn generic<T: Display>(x: T) -> String { cached_inner(x.to_string()) }
    #[cached] fn cached_inner(s: String) -> String { ... }
```

---

**#245** — @flavio

```
Hi @flavio,

Added in the upcoming major release. The release workflow now creates a git tag (`vX.Y.Z`)
and a GitHub release with auto-generated notes after each successful publish. Older releases
(v2.0.0 through v2.0.2) have been backfilled.
```

---

**#237** — @jqnatividad

```
Hi @jqnatividad,

Done in the upcoming major release. `sled` has been replaced with `redb` as the disk cache
backend. `redb` is actively maintained, compiles cleanly on current stable Rust, and uses
ACID transactions that eliminate the blob-leak bug (#206). Migration: swap
`DiskCacheBuilder` usage; the public API is unchanged.
```

---

**#20** — @cjbassi

```
Hi @cjbassi,

Disk caching has been available for a while and has been significantly improved in the
upcoming major release. `DiskCache` (sync) and `AsyncRedbCache` (async) are backed by redb
and support TTL, namespacing, and serde serialization. Closing.
```

---

## Bucket 3: defer

These are kept open. No reply drafted. A short note on why each is deferred.

| # | Title | Reporter | Note |
|---|-------|----------|------|
| [#239](https://github.com/jaemk/cached/issues/239) | Speed up compilation by replacing `darling` with `attrs` | @aatifsyed | Deferred to a later minor: meaningful compile-time improvement but large churn across all three macro arg structs; no user-visible breakage either way. |
| [#147](https://github.com/jaemk/cached/issues/147) | Update cached value asynchronously, outside the thread that returns the value | @kpears201 | Deferred: stale-while-revalidate (SWR) cluster (#147/#233/#228/#91) is a significant feature requiring a background task runtime; scoped out of this batch. |
| [#233](https://github.com/jaemk/cached/issues/233) | Using old cache while new data is being fetched | @NCura | Deferred: SWR cluster — same as #147. |
| [#228](https://github.com/jaemk/cached/issues/228) | stale-while-revalidate feature | @mharkins-cosm | Deferred: SWR cluster — same as #147. |
| [#91](https://github.com/jaemk/cached/issues/91) | Auto-refresh when remaining TTL is below a threshold | @gitmalong | Deferred: SWR cluster — same as #147. |
| [#222](https://github.com/jaemk/cached/issues/222) | Compression support | @buinauskas | Deferred to a later minor: adds a dependency and serialization complexity; no strong demand signal yet. |
| [#220](https://github.com/jaemk/cached/issues/220) | Add support with `moka`? | @xuxiaocheng0201 | Deferred to a later minor: moka is a quality cache but adding it as a backend is a significant integration effort. |
| [#32](https://github.com/jaemk/cached/issues/32) | adaptive replacement cache | @dvc94ch | Deferred to a later minor: ARC eviction policy is non-trivial and no maintained Rust ARC crate is a clear choice. |
| [#64](https://github.com/jaemk/cached/issues/64) | Supporting references | @szunami | Deferred: returning references into the cache is fundamentally a lifetime/borrow-checker problem that would require unsafe or an `Arc`-based API redesign. |
| [#188](https://github.com/jaemk/cached/issues/188) | Add helper attribute to ignore arguments | @ModProg | Deferred to a later minor: `convert` already covers this use case; a dedicated `ignore` attribute is a convenience improvement with no urgency. |
