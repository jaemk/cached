# `#[cached]` macro

Function-memoization attribute over a single-owner in-memory store, re-exported at
`cached::macros::cached` (feature `proc_macro`, on by default). Renamed from `cached::proc_macro`
pre-1.0; the Cargo feature is still `proc_macro`.

## CACHED-1

Core attributes: `name`, `max_size`, `ttl_secs` / `ttl_millis` / `ttl = "<Duration expr>"`,
`expires`, `refresh`, `ty`, `create`, `key`, `convert`, `cache_err`, `cache_none`,
`with_cached_flag`. `expires` selects per-value expiry via the `Expires` trait and is mutually
exclusive with the three TTL attributes. TTL
uses `ttl_secs` (whole seconds) / `ttl_millis` (ms) / `ttl` (a `Duration` expr), not `time =`;
refresh-on-hit is `refresh =`, not `time_refresh =`.

## CACHED-2

`Result<T, E>` / `Option<T>` returns skip caching `Err` / `None` by default; opt back in with
`cache_err = true` / `cache_none = true` (the pre-2.0 `result` / `option` attributes were
removed). `size = N` is a hard rename error directing to `max_size = N`, per
[design/0013-macro-store-attribute-placement.md](design/0013-macro-store-attribute-placement.md).
`unbound` is also a removed attribute: using it emits a compile error pointing to `#[cached]`
without `max_size`/`ttl`/`expires`, which already selects `UnboundCache`.

## CACHED-3

Write-synchronization attributes: `sync_writes` (`false`/`"false"`/`"disabled"` default = no
synchronization, `"by_key"` bucketed locks, `true`/`"true"`/`"default"` whole-cache lock),
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

## CACHED-6

`#[cached]` on a function whose return type is not `Clone` now emits one clear error: a
precisely-spanned `Clone`-bound assertion. The generated body is gated on that assertion, so the
follow-on E0308/E0599 cascade (previously 3 errors) no longer fires. Same change applies to
`#[once]`, see [macro-once.md](macro-once.md) ONCE-5. See [design/0043-macro-error-precision.md](design/0043-macro-error-precision.md).

## CACHED-7

The `size` -> `max_size` rename error (CACHED-2), the mutually-exclusive-TTL error, and the
generic-function-without-`key`/`convert` error (CACHED-1) now span the offending attribute rather
than the function name; the message text is unchanged, only the caret position improves. Shared
with `#[concurrent_cached]`, see [macro-concurrent-cached.md](macro-concurrent-cached.md) CONC-5.
See [design/0043-macro-error-precision.md](design/0043-macro-error-precision.md).

## CACHED-8

`companions` (bool, default `true`) suppresses the generated companions. `companions = false`
emits no `{fn}_prime_cache` and moves the `{fn}_no_cache` origin out of the parent module into a
function-local `fn` inside the cached function, so neither name can collide with a user item.
It composes with `in_impl = true`, which already suppresses `{fn}_prime_cache` and keeps
`{fn}_no_cache` as a sibling `impl` method (it takes `self`, which a nested `fn` cannot).
`companions_vis` with `companions = false` is a compile error off the `in_impl` path: there is
no companion left to apply it to. Same attribute on `#[once]` and `#[concurrent_cached]`, see
[macro-once.md](macro-once.md) ONCE-7 and
[macro-concurrent-cached.md](macro-concurrent-cached.md) CONC-6. See
[design/0024-generated-companion-naming.md](design/0024-generated-companion-naming.md).

The generated wrapper no longer emits `use ::cached::Cached;` or `use ::cached::CloneCached;`.
Both are replaced by fully-qualified trait paths, so a user item named after a `cached` trait
resolves to the user's item inside the function body and inside the `convert` / `force_refresh`
expressions, which the wrapper block also encloses.

## CACHED-9

`in_impl = true` requires a non-generic enclosing `impl`. An attribute macro applied to a method
receives only the method's tokens and cannot see the `impl` header, so the generic guard
(CACHED-1) does not fire on `impl<T> S<T> { #[cached(in_impl = true)] fn f(&self) -> T {..} }`:
the method's own generics are empty. The function-local cache static then names `T` and rustc
reports E0401 ("can't use generic parameters from outer item"; "a `static` is a separate item
from the item that contains it") on the return type. No detection is possible, so the limitation
is documented rather than diagnosed. Move the method to a non-generic `impl`, or memoize a free
function per concrete instantiation. Applies identically to `#[once]` and
`#[concurrent_cached]`. See
[design/0036-in-impl-static-placement.md](design/0036-in-impl-static-placement.md).

## CACHED-10

The concurrent-only attribute redirect (0013) covers `shards`, `durable`, `disk_dir`, and
`cache_prefix_block` in addition to `disk`, `redis`, and `map_error`. `#[cached(shards = 4)]`
now names the store the attribute configures and gives the `#[concurrent_cached]` spelling,
instead of darling's "Unknown field: `shards`". Same on `#[once]`. See
[design/0013-macro-store-attribute-placement.md](design/0013-macro-store-attribute-placement.md).
