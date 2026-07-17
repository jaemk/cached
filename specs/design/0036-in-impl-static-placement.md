# 0036 - in_impl static placement

Status: Implemented

## Current state

By default, `#[cached]` emits the cache static once at module scope, outside any function body,
with the function's visibility. The static's identifier (e.g. `FN_CACHE`) is visible in the
module namespace and accessible by the `{fn}_prime_cache` companion.

When `in_impl = true` is set, the macro is applied to a method inside an `impl` block. In that
case the cache static cannot be a module-level item because the generated code is emitted inside
the impl, and `static` is not valid as an impl item. Instead, the static is emitted inside the
generated function body (`cached_proc_macro/src/cached.rs:1237-1252`).

A function-local static is valid Rust (item-in-fn), initialized once on first call (same
semantics as a module static), and is only accessible from within that function body.

## Design decisions recorded here

**Function-local placement solves the same-name collision problem.** Without `in_impl`, two
methods with the same name on different types (e.g. `impl A { fn get(&self) }` and
`impl B { fn get(&self) }`) would both emit `GET_CACHE` at module scope. The second declaration
collides with the first. By placing the static inside the function body, each method has its own
distinct static even if the methods share a name. This fixes issues #16 and #140.

**Function-local statics carry no visibility modifier.** A `pub` or `pub(crate)` modifier on a
function-local item is meaningless (the item is not accessible outside the function body) and
triggers `unreachable_pub` on `deny(unreachable_pub)` builds. The generated static is emitted
bare (no visibility attribute), even though the module-scope path uses the function's visibility.

**Function-local statics carry no `#[doc]` attribute.** `rustdoc` ignores doc attributes on
function-local items; the attribute would be dead. The module-scope path includes a doc comment
for the static; the `in_impl` path omits it.

**The `{fn}_prime_cache` companion is not emitted under `in_impl`.** The prime companion would
need to reference the same function-local static. A static defined inside one function body is
not reachable from a sibling function body. Emitting a prime that cannot compile would be worse
than omitting it. The macro suppresses the prime silently; calling `method_prime_cache` on an
`in_impl`-annotated method is a compile error (undefined function), which is self-explanatory.

**`in_impl = true` is required (not inferred) on methods with `self`.** The macro rejects a
`self` receiver without `in_impl = true` with a clear error pointing at the attribute, and
rejects `in_impl = true` without a `self` receiver symmetrically. This keeps inference-free and
avoids surprises when a free function is renamed into a method.

## Notes

- `cached_proc_macro/src/cached.rs:259-285` contains the receiver/`in_impl` validation.
- `cached_proc_macro/src/cached.rs:1237-1252` contains the static placement branch.
- `cached_proc_macro/src/cached.rs:1261-1262` contains the prime suppression under `in_impl`.
- `cached_proc_macro/src/cached.rs:918-921` documents the visibility difference in a comment.
