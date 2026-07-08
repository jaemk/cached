# `#[concurrent_cached]` macro

Function-memoization attribute over a fully concurrent store with a shared `&self` API,
re-exported at `cached::macros::concurrent_cached` (feature `proc_macro`). Backs the sharded
in-memory stores by default, and the redis/redb backends when so configured.

## CONC-1

Store selection follows the attributes: no extra attrs -> `ShardedUnboundCache`; `max_size` ->
sharded LRU; `ttl_secs` / `ttl_millis` / `ttl` -> sharded TTL; `expires = true` -> sharded
expiring; combinations pick the LRU+TTL / LRU+expiring variant. See
[store-sharded.md](store-sharded.md).

## CONC-2

Shares the core attributes with `#[cached]` (`name`, `max_size`, `ttl_*`, `refresh`, `ty`,
`create`, `key`, `convert`, `cache_err`, `cache_none`, `with_cached_flag`) but does not support
`sync_writes` (the concurrent stores self-synchronize).

## CONC-3

For disk/redis stores, `map_error` (unquoted closure `|e| MyErr(e)` or quoted string) converts
the store error into the function's error type. When omitted, `.map_err(Into::into)?` is
generated, requiring `E: From<StoreError>`. Store errors are named per
[design/0005-store-error-consistency.md](design/0005-store-error-consistency.md); unifying
single-variant argument errors is an open direction
([design/0020-argument-error-unification.md](design/0020-argument-error-unification.md)).
