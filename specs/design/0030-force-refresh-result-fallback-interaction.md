# 0030 - force_refresh and result_fallback interaction

Status: Implemented

## Current state

`#[cached]` supports two independent attributes that interact when used together:

**`result_fallback = true`**: when the function returns `Err(...)`, the macro returns the last
cached `Ok` value instead. Requires a `Result<T, E>` return type and a store implementing
`CloneCached`. With no `force_refresh`, the macro performs a renewing `cache_get_with_expiry_status`
read (updates LRU recency, hit count, and TTL on refresh-on-hit stores) on the fast path, and
stores the captured value as the fallback.

**`force_refresh = "{ expr }"`**: a boolean expression over the function arguments that, when
true, bypasses the cached value and always runs the body. The result (if `Ok`) is stored.

## Design decisions recorded here

**`force_refresh` bypasses the entry using a non-renewing peek.** When `force_refresh` is set
alongside `result_fallback`, the macro captures the fallback value with
`cache_peek_with_expiry_status` instead of `cache_get_with_expiry_status` on the bypass path.
This ensures a bypassed read has no side effects: no LRU promotion, no hit-count increment, no
TTL renewal (`src/cached_proc_macro/src/cached.rs:1109-1135`). Peeking also captures expired
entries so an `Err` recompute over an expired key still falls back to the stale `Ok` value.

**The force_refresh predicate is evaluated once per call.** The generated code stores
`__cached_force_refreshing` from a single evaluation of the predicate and reuses it for both the
skip-read branch and the skip-store branch, so the expression is not evaluated twice.

**`result_fallback` and explicit non-Disabled `sync_writes` are mutually exclusive.** The two
attributes conflict: `result_fallback` needs a peek before the call and a conditional store after,
which is incompatible with the `by_key` lock structure. A compile error is emitted if both are
set with a non-Disabled sync mode. When `sync_writes` is not set explicitly and `result_fallback`
is set, `sync_writes` is forced to `Disabled` (`cached_proc_macro/src/cached.rs:781-794`).

**`result_fallback` is also mutually exclusive with `cache_err` and `with_cached_flag`.** Both
conflicts are compile errors. `cache_err` caches the error value directly, which is the opposite
of the fallback contract. `with_cached_flag` injects a `was_cached()` boolean that has no
well-defined meaning when the returned value may come from either the function or the store.

**`result_fallback` requires `CloneCached`.** The fallback value is captured by clone before the
function runs, so the store must implement `CloneCached`. Without an explicit `ty`, the macro also
requires either `ttl`/`ttl_millis`/`ttl_secs` or `expires = true`, because `CloneCached` is only
available on TTL-capable and expiring stores.

## Notes

- `cached_proc_macro/src/cached.rs:1104-1169` contains the full codegen for this interaction.
- `force_refresh` alone (without `result_fallback`) uses a simpler guard pattern that short-circuits
  the lookup unconditionally when the predicate is true.
