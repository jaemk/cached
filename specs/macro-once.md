# `#[once]` macro

Attribute that memoizes a single value shared across all calls, ignoring arguments in the cache
key. Re-exported at `cached::macros::once` (feature `proc_macro`).

## ONCE-1

The generated static holds one concrete value type, so `#[once]` functions may be generic without
the `key` + `convert` pinning that `#[cached]` requires. See [macro-cached.md](macro-cached.md).

## ONCE-2

Attributes: `name`, `ttl_secs` / `ttl_millis` / `ttl`, `cache_err`, `cache_none`,
`with_cached_flag`, `expires`, `force_refresh`, `in_impl`, `companions_vis`. `sync_writes`
defaults to `false`. There is no `refresh =` attribute (a single-value cache has no per-key
refresh-on-hit).

## ONCE-3

`Result` / `Option` returns skip caching `Err` / `None` by default; opt in with
`cache_err = true` / `cache_none = true`.

## ONCE-4

Emits `foo_prime_cache(..)` keeping the function's own arguments (the body runs to prime the
single stored value; arguments do not affect the key). The prime companion runs the body before
taking the lock, per
[design/0034-prime-companion-body-before-lock.md](design/0034-prime-companion-body-before-lock.md).

## ONCE-5

`#[once]` on a function whose return type is not `Clone` now emits one clear error: a
precisely-spanned `Clone`-bound assertion. The generated body is gated on that assertion, so the
follow-on E0308/E0599 cascade (previously 5 errors) no longer fires. Same change applies to
`#[cached]`, see [macro-cached.md](macro-cached.md) CACHED-6. See [design/0043-macro-error-precision.md](design/0043-macro-error-precision.md).

## ONCE-6

The mutually-exclusive-TTL error (shared with `#[cached]`/`#[concurrent_cached]` via the same
resolution helper) now spans the offending attribute rather than the function name; the message
text is unchanged. `#[once]` has no `max_size` or generic-`key`/`convert` errors to re-span (see
ONCE-1). See [design/0043-macro-error-precision.md](design/0043-macro-error-precision.md).

## ONCE-7

`companions` (bool, default `true`) suppresses the generated companions. `#[once]` already
emits the `{fn}_no_cache` origin as a function-local `fn` inside the cached function off the
`in_impl` path, so `companions = false` drops `{fn}_prime_cache` (ONCE-4). It composes with
`in_impl = true`, which already suppresses `{fn}_prime_cache` and keeps `{fn}_no_cache` as a
sibling `impl` method. `companions_vis` with `companions = false` is a compile error off the
`in_impl` path. Shared with `#[cached]`, see [macro-cached.md](macro-cached.md) CACHED-8. See
[design/0024-generated-companion-naming.md](design/0024-generated-companion-naming.md).

## ONCE-8

`in_impl = true` requires a non-generic enclosing `impl`, and the concurrent-only attribute
redirect covers `shards`, `durable`, `disk_dir`, and `cache_prefix_block`. Both are shared with
`#[cached]`, see [macro-cached.md](macro-cached.md) CACHED-9 and CACHED-10. The G1 guard
(ONCE-1) inspects only the function's own generics, so it cannot see an `impl` parameter that
the value type names.
