# 0043 - Macro error precision: gate the generated body on the `Clone` assert, span attribute errors at the attribute

Status: Implemented

Two diagnostics changes to the proc macros. Neither changes which programs compile or what any
message says; both change how many errors a bad program produces and where the caret lands.

## (a) One error for a non-`Clone` return type

### Previous state

`#[cached]` and `#[once]` require the cached value type to be `Clone`, since the cache stores the
value and hands out clones. `clone_return_assertion` emitted a precisely-spanned check for this: a
local `fn __cached_assert_return_type_implements_clone<T: Clone>() {}` plus a call naming the
return type in its turbofish, so the error underlined the user's return type.

The assertion was emitted alongside the generated body, not in front of it. When the return type
was not `Clone` the assertion fired and so did every `.clone()` / `.to_owned()` call in the
generated body, producing an E0308 / E0599 cascade whose spans point at the attribute itself
(macro-generated code has nowhere better to point). The committed goldens recorded 3 errors for
each of `#[cached]`, `#[cached(with_cached_flag)]`, and `#[once]`. The precise one is first, but
the reader has to work out that the following errors are consequences of it rather than
independent problems, and they name macro-internal expressions the user never wrote.

The `clone_return_assertion` doc comment claimed the assertion made the clear error appear in
place of the cascade. It did not: it appeared before it.

### The rule

A non-`Clone` return type produces exactly one error, precisely spanned at the return type.

Gating the generated body on a separate assertion was tried first and does not reach one error.
The assertion helper is function-local, so the `_prime_cache` companion body needs its own copy,
and rustc's diagnostic deduplication then fails on the differing note paths
(`foo::__cached_assert...` vs `foo_prime_cache::__cached_assert...`), leaving 2 errors.

What ships instead: `clone_return_assertion` is replaced by `clone_cached_value`
(`cached_proc_macro/src/helpers.rs`), which emits every clone of the cached value as a qualified
call, `<Ret as ::std::clone::Clone>::clone(expr)`, `quote_spanned!` at the return type. That
single construct is both the bound assertion and the clone, so there is no method left to resolve
(no E0599) and no `&T` / `T` mismatch (no E0308). Every clone site in every generated body renders
an identical diagnostic, which rustc then deduplicates to one. All `.clone()` / `.to_owned()`
sites in `cached.rs` and `once.rs` route through it.

## (b) Span attribute errors at the offending attribute

### Previous state

Three `#[cached]` attribute validations built their `syn::Error` with `fn_ident.span()`:

- the `size` -> `max_size` rename error (`cached_proc_macro/src/cached.rs:400`),
- the mutually-exclusive TTL attribute errors (`ttl` / `ttl_secs` / `ttl_millis`, and the
  `expires` pairings),
- the generic-function-without-`key`/`convert` error (`cached_proc_macro/src/cached.rs:300`).

Each message text is good and actionable, and each says exactly what to do. The caret, however,
underlined the function's name, which is the one token in the item that is not the problem. On a
function with several attributes the reader has to re-read the attribute list to find the one the
message is about.

### The rule

Each of these errors is spanned at the offending attribute (the `size = N` token, the conflicting
TTL attribute, the attribute list of a generic function missing `key`/`convert`) instead of at the
function name. The message text is unchanged; only the span moves.

## Observable surface that changes

Compile diagnostics only. No change to accepted attributes, generated code, or runtime behavior.

- A non-`Clone` return type yields 1 error instead of 3, on each of `#[cached]`,
  `#[cached(with_cached_flag)]`, and `#[once]`.
- The three attribute errors underline the attribute rather than the function name.
- The `tests/ui/` golden files record both, so the error count and the caret position are pinned
  and a regression shows up as a `.stderr` diff. Regenerate with
  `TRYBUILD=overwrite cargo test --features "proc_macro,time_stores"`.

## Notes

- (a) and (b) are grouped in one record because they are the same kind of change: the macro
  already knew the right thing to say, and only the presentation (how many errors, and where they
  point) was wrong.
- The assertion technique changed: the separate local-fn assertion is gone, replaced by the
  qualified `<Ret as Clone>::clone` call described in (a). See the `clone_cached_value` doc
  comment for its handling of generic parameters in scope.
- See [macro-cached.md](../macro-cached.md) and [macro-once.md](../macro-once.md) for the shipped
  behavior.
