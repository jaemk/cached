# 0024 - Rename or namespace generated companion fns

Status: Opt-out implemented; rename declined

## Current state

- All three macros emit `{fn}_no_cache` and `{fn}_prime_cache` alongside the cached function.
  `#[cached]` emits both as free functions in the parent module; `#[once]` and
  `#[concurrent_cached]` nest `{fn}_no_cache` inside the cached function body and emit only
  `{fn}_prime_cache` into the module. Either way a generated name can collide with a user
  function.
- `_prime_cache` is tagged `#[allow(dead_code)]`, an admission it is often unused.
- `companions = false` suppresses the companions on all three macros. The names are unchanged.

## Design decisions recorded here

**The opt-out switch is `companions`, a bool, defaulting to `true`.** `companions = false`
emits no `{fn}_prime_cache`, and on `#[cached]` moves the `{fn}_no_cache` origin out of the
parent module into a function-local `fn` inside the cached function. The default is the
existing behavior, so the attribute is purely additive.

**The origin body moves into a nested `fn`, not inline into the wrapper.** The body still has
to live somewhere. A nested `fn` keeps `return` and `?` bound to the user's own body; inlining
the block into the wrapper would silently rebind them to the wrapper and skip the cache-set on
an early return. `#[once]` and `#[concurrent_cached]` already generate the origin this way off
the `in_impl` path, so `companions = false` makes `#[cached]` consistent with them rather than
introducing a new shape.

**`companions = false` composes with `in_impl = true`; it does not conflict.** `in_impl`
already suppresses `{fn}_prime_cache` (the cache static is function-local and cannot be shared
with a sibling), so the two agree there. `{fn}_no_cache` stays a sibling `impl` method under
`in_impl`, because it takes a `self` receiver and a function-local `fn` cannot have one. That
is not a gap: an inherent method is namespaced by its type and cannot collide with a free
function in the parent module, which is the collision this switch exists to avoid. Together the
two attributes remove every companion item the macro is structurally able to omit.

**No generated `use` may put a name in a scope that encloses user code.** `#[cached]` used
to emit `use ::cached::Cached;` (and `use ::cached::CloneCached;` under `result_fallback`)
into the wrapper's block. That block already enclosed the user's `convert` and
`force_refresh` expressions, and `companions = false` moves the whole function body into it,
so the import shadowed any user item of the same name: a `struct Cached` in the body
resolved to the trait and failed with E0782. Both imports are gone; every store call now
goes through a fully-qualified `#krate::Cached::cache_set(&mut *__cached_cache, ..)` /
`#krate::CloneCached::cache_get_with_expiry_status(..)` path, matching the
`#krate::CachedRead::cache_get_read` and `#krate::CachedPeek::cache_peek` calls the codegen
already used. The one remaining generated import is the `use ... as _;` autoref shim in
`#[concurrent_cached]`'s `set_call`, which binds no name and so cannot shadow anything.

The qualified paths also sharpen the store diagnostics, in line with
[0043](0043-macro-error-precision.md). A value type that fails the store's bound (e.g.
`#[cached(expires = true)]` on a non-`Expires` type) used to produce one E0277 plus two E0599s
naming `lock_api::rwlock::RwLockWriteGuard<'_, parking_lot::raw_rwlock::RawRwLock, _>`, a type
the user never wrote. It now produces one E0277 plus one E0277 that names the real unsatisfied
bound and points at the `Cached` impl.

**`companions_vis` with `companions = false` is a compile error off the `in_impl` path.**
There is no companion item left for the visibility to apply to, so the value would be silently
discarded. Under `in_impl` the `{fn}_no_cache` sibling survives and still takes it, so that
pairing stays valid.

**The rename half is declined for 3.0.** Renaming to `{fn}_uncached`/`{fn}_prime`, or
namespacing both under a generated `{fn}_cache` module, would churn every call site of both
companions in every downstream crate for a modest naming gain, and `companions = false` already
addresses the collision complaint that motivated the record. A rename is a breaking change and
is now 4.0-only.

## Notes

- A module namespace remains the cleaner end state but a bigger break than a rename. If it is
  revisited for 4.0, pick one scheme and apply it identically to
  `#[cached]`/`#[once]`/`#[concurrent_cached]`; migration is a call-site rename.
- `tests/companions_optout.rs` covers the switch, the unchanged default, the `in_impl`
  composition, and the compile-fail cases proving each companion is absent.
