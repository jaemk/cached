# `#[once]` macro

Attribute that memoizes a single value shared across all calls, ignoring arguments in the cache
key. Re-exported at `cached::macros::once` (feature `proc_macro`).

## ONCE-1

The generated static holds one concrete value type, so `#[once]` functions may be generic without
the `key` + `convert` pinning that `#[cached]` requires. See [macro-cached.md](macro-cached.md).

## ONCE-2

Attributes: `name`, `ttl_secs` / `ttl_millis` / `ttl`, `cache_err`, `cache_none`,
`with_cached_flag`. `sync_writes` defaults to `false`. There is no `refresh =` attribute (a
single-value cache has no per-key refresh-on-hit).

## ONCE-3

`Result` / `Option` returns skip caching `Err` / `None` by default; opt in with
`cache_err = true` / `cache_none = true`.

## ONCE-4

Emits `foo_prime_cache(..)` keeping the function's own arguments (the body runs to prime the
single stored value; arguments do not affect the key). The prime companion runs the body before
taking the lock, per
[design/0034-prime-companion-body-before-lock.md](design/0034-prime-companion-body-before-lock.md).
