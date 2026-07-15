# `#[cached]` macro

Function-memoization attribute over a single-owner in-memory store, re-exported at
`cached::macros::cached` (feature `proc_macro`, on by default). Renamed from `cached::proc_macro`
pre-1.0; the Cargo feature is still `proc_macro`.

## CACHED-1

Core attributes: `name`, `max_size`, `ttl_secs` / `ttl_millis` / `ttl = "<Duration expr>"`,
`refresh`, `ty`, `create`, `key`, `convert`, `cache_err`, `cache_none`, `with_cached_flag`. TTL
uses `ttl_secs` (whole seconds) / `ttl_millis` (ms) / `ttl` (a `Duration` expr), not `time =`;
refresh-on-hit is `refresh =`, not `time_refresh =`.

## CACHED-2

`Result<T, E>` / `Option<T>` returns skip caching `Err` / `None` by default; opt back in with
`cache_err = true` / `cache_none = true` (the pre-2.0 `result` / `option` attributes were
removed). `size = N` is a hard rename error directing to `max_size = N`, per
[design/0013-macro-store-attribute-placement.md](design/0013-macro-store-attribute-placement.md).

## CACHED-3

Write-synchronization attributes: `sync_writes` (`false`/`"disabled"` default = no
synchronization, `"by_key"` bucketed locks, `true`/`"default"` whole-cache lock),
`sync_writes_buckets` (default 64; a compile error unless `sync_writes = "by_key"`),
`sync_lock` (`"rwlock"` default or `"mutex"`), `unsync_reads` (shared read lock for hits;
`CachedRead` stores only). The `false` default and the earlier revert are recorded in
[design/0027-sync-writes-default-revert.md](design/0027-sync-writes-default-revert.md); the
per-key lock buckets use a seeded hasher per
[design/0035-seeded-per-key-lock-bucket-hasher.md](design/0035-seeded-per-key-lock-bucket-hasher.md).

## CACHED-4

Behavior attributes: `result_fallback` (return the last cached `Ok` on `Err`; requires
`Result`), `force_refresh` (bypass and recompute when a bool expr over the args is true),
`in_impl` (generate a `_no_cache` sibling with a function-local static; suppresses
`_prime_cache`), `companions_vis`. The `force_refresh` / `result_fallback` interaction is
specified in
[design/0030-force-refresh-result-fallback-interaction.md](design/0030-force-refresh-result-fallback-interaction.md);
`in_impl` static placement in
[design/0036-in-impl-static-placement.md](design/0036-in-impl-static-placement.md).

## CACHED-5

Every generated `foo(..)` also emits `foo_prime_cache(..)` (bypass + force re-execution),
except `in_impl` methods. The prime companion runs the body before taking the lock, per
[design/0034-prime-companion-body-before-lock.md](design/0034-prime-companion-body-before-lock.md).
Generic functions with `where` clauses are supported; a generic that lands in the key/value type
must be pinned via `key` + `convert` + `ty`. Companion naming is an open direction
([design/0024-generated-companion-naming.md](design/0024-generated-companion-naming.md)); quoted
string attributes were retained
([design/0006-macro-quoted-attributes.md](design/0006-macro-quoted-attributes.md), declined).
