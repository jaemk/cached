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
