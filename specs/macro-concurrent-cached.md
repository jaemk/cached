# `#[concurrent_cached]` macro

Function-memoization attribute over a fully concurrent store with a shared `&self` API,
re-exported at `cached::macros::concurrent_cached` (feature `proc_macro`). Backs the sharded
in-memory stores by default, and the redis/redb backends when so configured.

## CONC-1

Store selection follows the attributes:

| Attributes | Store |
|---|---|
| (none) | `ShardedUnboundCache` |
| `max_size` | `ShardedLruCache` |
| `ttl_secs` / `ttl_millis` / `ttl` | `ShardedTtlCache` |
| `max_size` + TTL | `ShardedLruTtlCache` |
| `expires = true` | `ShardedExpiringCache` |
| `expires = true` + `max_size` | `ShardedExpiringLruCache` |

See [store-sharded.md](store-sharded.md).

## CONC-2

Shares the core attributes with `#[cached]` (`name`, `max_size`, `ttl_*`, `refresh`, `ty`,
`create`, `key`, `convert`, `cache_err`, `cache_none`, `with_cached_flag`) but does not support
`sync_writes` (the concurrent stores self-synchronize). Additional attributes:
`force_refresh`, `in_impl`, `companions_vis`, `result_fallback`, `expires`, `shards` (default
in-memory store only), `redis`, `disk`, `disk_dir`, `durable`, `cache_prefix_block`
(redis/disk paths).

## CONC-3

For disk/redis stores, `map_error` (closure, e.g. `|e| MyErr(e)`) converts
the store error into the function's error type. When omitted, a bare `?` is generated, which
converts through `From` and so requires `E: From<StoreError>` (an explicit
`.map_err(Into::into)` is deliberately not emitted: it is ambiguous when the target error has
multiple `From` impls). Store errors are named per
[design/0005-store-error-consistency.md](design/0005-store-error-consistency.md); unifying
single-variant argument errors is an open direction
([design/0020-argument-error-unification.md](design/0020-argument-error-unification.md)).

## CONC-4

`#[concurrent_cached]` gains compile-time missing-feature guards for the disk and redis backends,
mirroring the existing `time_stores` guard (CONC-1) and the `async` feature guard. Without the
`redb_store` feature, `#[concurrent_cached(disk = true, ...)]` now emits one `compile_error!`
naming `redb_store`, in place of raw E0433/E0425 errors ("cannot find `RedbCache` in `cached`").
Without a redis feature, `#[concurrent_cached(redis = true, ...)]` now emits one `compile_error!`
naming the redis features (`redis_tokio` / `redis_smol` and their TLS variants), in place of the
async feature guard firing instead: previously that guard both named the wrong feature
(`async_core`) and leaked the doc-hidden internal `__set_dispatch_async` path into the error. The
async guard is ordered so it cannot pre-empt the redis guard. See
[design/0042-macro-feature-guard-errors.md](design/0042-macro-feature-guard-errors.md) and
[cargo-features.md](cargo-features.md) FEAT-8.

## CONC-5

The `size` -> `max_size` rename error, the mutually-exclusive-TTL error, and the
generic-function-without-`key`/`convert` error (shared with `#[cached]`, see
[macro-cached.md](macro-cached.md) CACHED-7) now span the offending attribute rather than the
function name; the message text is unchanged. See [design/0043-macro-error-precision.md](design/0043-macro-error-precision.md).

## CONC-6

`companions` (bool, default `true`) suppresses the generated companions.
`#[concurrent_cached]` already emits the `{fn}_no_cache` origin as a function-local `fn` inside
the cached function off the `in_impl` path, so `companions = false` drops `{fn}_prime_cache`.
It composes with `in_impl = true`, which already suppresses `{fn}_prime_cache` and keeps
`{fn}_no_cache` as a sibling `impl` method. `companions_vis` with `companions = false` is a
compile error off the `in_impl` path. Shared with `#[cached]`, see
[macro-cached.md](macro-cached.md) CACHED-8. See
[design/0024-generated-companion-naming.md](design/0024-generated-companion-naming.md).

## CONC-7

`result_fallback` accepts `expires = true` as well as a TTL, matching `#[cached]`. The
requirement is entries that expire, by a uniform TTL (`ShardedTtlCache` /
`ShardedLruTtlCache`) or per value (`ShardedExpiringCache` / `ShardedExpiringLruCache`, CONC-1);
all four implement `ConcurrentCloneCached`, which supplies the expiry-aware reads the
`result_fallback` codegen performs. The previous "`result_fallback` and `expires` are mutually
exclusive" rejection is removed, and the no-expiry error now names both options. Stale-value
semantics match `#[cached(expires = true, result_fallback = true)]`: the returned fallback is
the expired value itself, so callers that must tell a fresh result from a stale one check the
value's own `Expires::is_expired`. See
[design/0030-force-refresh-result-fallback-interaction.md](design/0030-force-refresh-result-fallback-interaction.md).

## CONC-8

`in_impl = true` requires a non-generic enclosing `impl`; the guard cannot see the `impl`
header. Shared with `#[cached]`, see [macro-cached.md](macro-cached.md) CACHED-9.
