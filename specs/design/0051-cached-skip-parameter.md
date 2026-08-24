# 0051 - `#[cached_skip]`: exclude a parameter from the generated cache key

Status: Not implemented

## Current state

Issue #188 ("Add helper attribute to ignore arguments") asks for a per-parameter marker so one
or two arguments can be left out of the cache key without restating the whole key by hand:

```rust
#[cached]
fn hello(a: usize, #[cached(skip)] b: String)
```

The issue does not ask for anything beyond that - no rename/alias behavior, no interaction with
`ty`/`create`. One +1 comment, no maintainer reply yet.

Today's only workaround is `key` + `convert`, restating the kept parameters and suppressing an
unused-variable warning on the dropped one. The README documents exactly this shape at
`README.md:157` (and its `#[concurrent_cached]` mirror at `README.md:180`):

```
#[cached(key = "u64", convert = { id }, force_refresh = { refresh })] fn fetch(id: u64, refresh: bool) -> Data { let _ = refresh; ... }
```

`convert` here is written by hand and does not consult the parameter list at all, so this
workaround does not scale: every kept parameter has to be re-listed in the `convert` block, and
the block has no way to know if the function signature changes later.

### Key construction sites (verified against current source)

`make_cache_key_type` (`cached_proc_macro/src/helpers.rs:522-594`) has three arms, matched on
`(key, convert, ty)`:

- `(Some(key_str), Some(convert_expr), _)` at `helpers.rs:530-545` - the `key`/`convert` arm.
  It parses `key_str` into the cache key type and lowers `convert_expr` into the key-conversion
  block. It never looks at `input_tys`/`input_names` at all; the key type and the conversion
  expression are entirely user-written. (The shard spec cited 530-544; the arm's closing brace is
  545, so the corrected range is 530-545.)
- `(None, Some(convert_expr), Some(_))` at `helpers.rs:546-550` - `ty` supplies the key type,
  `convert` supplies the block. Same property: does not consult input types.
- `(None, None, _)` at `helpers.rs:551-585` - the default-key path (verified; the shard spec's
  551-584 is off by one, the arm's closing brace is 585). This is the only arm that derives the
  key from the function's own parameters: it zips `input_tys` and `input_names`
  (`helpers.rs:563`) and, per parameter, decides between a `ToOwned`-derived owned type
  (`&T`/`&mut T`/`Option<&T>`/`Option<&mut T>`) and a plain `.clone()` (`helpers.rs:564-579`),
  then joins everything into one tuple key type and one tuple key expression
  (`helpers.rs:581-584`).

`input_tys`/`input_names` come from two sibling helpers, both filtering out the receiver the same
way:

- `get_input_names` (`helpers.rs:636-647`) - filters `FnArg::Receiver` to `None`, so `self` is
  never part of the key. This is the existing precedent for excluding something from the key by
  position.
- `get_input_types` (`helpers.rs:660-669`) - the same filter, over `pat_type.ty` instead of the
  pattern.

Both are position-aligned (receiver-filtered index `i` in one lines up with index `i` in the
other), which the default-key path at `helpers.rs:563` relies on by zipping them directly.

`get_mut_signature` (`helpers.rs:356-373`, verified exact) is the mut-stripping pass on a
`Signature`: it walks every `FnArg`, leaves `FnArg::Receiver` untouched, and for
`FnArg::Typed(pat_type)` clones the `PatType` and replaces `pt.pat` with the mut-stripped pattern
from `match_pattern_type`. It is the shape a parameter-attribute-stripping pass should follow
(same walk, same `FnArg::Typed` match arm), and per-macro it is the single choke point every
downstream *mut-stripped* signature already flows through.

### Where each raw `Signature` gets cloned

Each of the three macros parses one `syn::Signature` (`let signature = input.sig;` -
`cached.rs:209`, `concurrent_cached.rs:230`, `once.rs:157`) and clones it into at least two
outputs: the full-signature companion (`_no_cache` / `__cached_inner` / `<fn>_no_cache`) and,
via `get_mut_signature`, the mut-stripped public wrapper and `_prime_cache` companion.
Verified clone sites, corrected against the shard spec below:

| Macro | Raw `signature.clone()` (companion with the *original*, unstripped signature) | `get_mut_signature(signature)` (feeds the public wrapper + `_prime_cache`) |
|---|---|---|
| `#[cached]` | `cached.rs:999` (`no_cache_sig`) | `cached.rs:1294` -> `prime_sig` at `cached.rs:1298`; wrapper at `cached.rs:1441` |
| `#[concurrent_cached]` | `concurrent_cached.rs:1144` (`inner_sig`) | `concurrent_cached.rs:1285` -> `prime_sig` at `concurrent_cached.rs:1289`; wrapper at `concurrent_cached.rs:1499`/`1520` |
| `#[once]` | `once.rs:747` (`inner_sig`) | `once.rs:939` -> `prime_sig` at `once.rs:942`; wrapper at `once.rs:1032` |

Correction: the shard spec cited `concurrent_cached.rs:1285-1290` as the raw-clone site that
"needs its own strip". That range is actually the `get_mut_signature`/`prime_sig` pair, not a raw
`signature.clone()`. The raw clone in `concurrent_cached.rs` is at line 1144
(`let mut inner_sig = signature.clone();`), the direct analog of `cached.rs:999` and
`once.rs:747`. All three raw-clone sites (999, 1144, 747) and all three `get_mut_signature` call
sites (1294, 1285, 939) read from the same local `signature` binding in their file, so - see
Pitfalls - a single strip performed on `signature` before any of these six lines run covers all
of them; there is no need for six independent strip calls.

`input_names` is reused for a second purpose beyond key construction: it is also the argument
list forwarded to the `_no_cache`/`inner`/origin function call
(`cached.rs:1024`/`1032`, `concurrent_cached.rs:` companion call sites, `once.rs:811`/`837`).
That reuse is why the key-filtering step cannot simply mutate `input_names` in place (see
Pitfalls).

### The generic-function guard does not change

`#[cached]` on a generic function is rejected unless `key`+`convert` is given, because the cache
is one monomorphic `static` (`cached.rs:348-364`, verified exact; `#[concurrent_cached]`'s mirror
is `concurrent_cached.rs:325-341`, verified exact). This guard reads `signature.generics`, not
the parameter list, and `#[cached_skip]` does nothing to it: a `skip`ped parameter is still part
of the function signature and can still carry a type parameter that appears nowhere in the key,
so the monomorphization problem is unchanged. `#[cached_skip]` does not unlock caching generic
functions.

### `#[once]` has no key at all

`#[once]` (`once.rs`) stores one value for all calls; it has `key`/`convert` fields in
`OnceMacroArgs` (`once.rs:101`, `once.rs:103`) purely to reject them with a clear message
(`once.rs:392-409`, verified exact: `key` at 392-399, `convert` at 401-408). `input_names` in
`once.rs` (computed at `once.rs:446`, verified exact) is used only to forward the call to the
origin function (`once.rs:811`/`837`); there is no `get_input_types` call and no
`make_cache_key_type` call anywhere in `once.rs`. `#[cached_skip]` on an `#[once]`-decorated
parameter has nothing to exclude from and must be rejected the same way `key`/`convert` already
are.

## Desired work

### Attribute spelling: `#[cached_skip]`, a bare marker, not `#[cached(skip)]`

The issue's own suggestion, `#[cached(skip)]`, reuses the crate's own attribute name as a
sub-namespace, mirroring `#[serde(skip)]`/`#[sqlx(skip)]`. That convention works for a *derive*
macro, which can declare `#[proc_macro_derive(Foo, attributes(serde))]` and have rustc treat
`#[serde(...)]` on a field as an inert helper attribute it is allowed to leave in place. None of
this crate's three macros are derive macros - `cached`, `once`, and `concurrent_cached` are all
`#[proc_macro_attribute]` (`cached_proc_macro/src/lib.rs:169`, `:246`, `:489`), and attribute
macros have no equivalent `attributes(...)` registration. There is no mechanism by which rustc
would ever recognize `#[cached(skip)]` on a parameter as legal syntax on its own; it is legal
only because it sits inside the *unexpanded* token stream the outer `#[cached]`/`#[once]`/
`#[concurrent_cached]` invocation receives, and it is the macro's sole responsibility to consume
it before any of that signature is quoted back out. If a stray copy survives into the returned
`TokenStream` (see Pitfalls), rustc will try to resolve it as a genuine attribute invocation on a
function parameter and fail - there is no silent fallback.

Given that, reusing `cached` as the sub-namespace is actively misleading on the two macros that
are not named `cached`: a parameter under `#[once(...)]` or `#[concurrent_cached(...)]` marked
`#[cached(skip)]` reads as an accidental second invocation of the *other* macro, not a shared
family marker. Recommendation: a single bare marker attribute, independent of which of the three
outer macros is present, spelled `#[cached_skip]`:

```rust
#[cached]
fn hello(a: usize, #[cached_skip] b: String) -> String { ... }
```

Bare (no `(...)`, no darling parsing) because the feature is a single boolean per parameter with
no configuration - matching the simplicity of the existing bare per-parameter precedent this
crate already has none of, but consistent with how the macros treat other zero-argument toggles.
Detection is a plain `attr.path().is_ident("cached_skip")` scan over `pat_type.attrs`, no new
dependency.

`#[cached_skip]` on `#[once]` and on the `key`/`convert` arm are both hard errors (see below), so
in practice the marker only ever has an effect on `#[cached]` and `#[concurrent_cached]`'s
default-key path. It is still recognized (and stripped) uniformly by all three macros so that the
same rejection path can name it consistently in error text, rather than three macros disagreeing
on what a `skip`ped parameter under the "wrong" macro is even called.

### Implementation shape: one strip pass per macro, not one per clone site

Do not chase every `signature.clone()` individually. Add one function to `helpers.rs`, next to
`get_mut_signature`, that walks `signature.inputs` the same way `get_mut_signature` does
(`FnArg::Receiver` untouched, `FnArg::Typed(pat_type)` rebuilt) and, in the same pass:

1. Records, per non-receiver parameter and in the same receiver-filtered order as
   `get_input_names`/`get_input_types`, whether `#[cached_skip]` was present (`Vec<bool>`,
   position-aligned with `input_names`/`input_tys`).
2. Removes any `#[cached_skip]` attribute from `pat_type.attrs` so the returned `Signature`'s
   parameters carry no trace of it.

Call this once, in each of `cached.rs`/`concurrent_cached.rs`/`once.rs`, immediately after
`let signature = input.sig;` (i.e. before `let inputs = signature.inputs.clone();` at
`cached.rs:250`/`concurrent_cached.rs:254`/`once.rs:186`) and change `signature` to `mut` there.
Every later use of `signature` in that file - `inputs.clone()`, the raw-clone companions
(`cached.rs:999`, `concurrent_cached.rs:1144`, `once.rs:747`), and `get_mut_signature(signature)`
(`cached.rs:1294`, `concurrent_cached.rs:1285`, `once.rs:939`) - then reads the already-stripped
signature for free, because they all read from the same local binding this early strip mutated.
This turns "six clone sites, each needing its own strip" into "one strip call per file, placed
before the first clone." Order in the file matters; see Pitfalls for what happens if the strip
runs after `inputs` is cloned.

### Order of changes

1. `helpers.rs`: add the strip-and-collect-mask pass next to `get_mut_signature`
   (`helpers.rs:356-373`).
2. `cached.rs`: call it right after `let signature = input.sig;` (`cached.rs:209`), before
   `inputs.clone()` (`cached.rs:250`). Reject `#[cached_skip]` combined with `key`/`convert`
   (see below) once `args`/`CachedMacroArgs` and the mask are both available. Build a *separate*
   filtered `(key_input_tys, key_input_names)` for the `make_cache_key_type` call at
   `cached.rs:689` - do not filter the existing `input_tys`/`input_names` bindings in place (see
   Pitfalls).
3. `concurrent_cached.rs`: mirror step 2 (`concurrent_cached.rs:230`/`254`/`765`).
4. `once.rs`: call the strip pass right after `let signature = input.sig;`
   (`once.rs:157`), purely so the marker never leaks into `once.rs`'s own signature clones
   (`once.rs:747`, `:939`). Then reject any skip-marked parameter unconditionally, following the
   existing `key`/`convert` rejection pattern (`once.rs:392-409`), placed next to it.
5. `README.md`: add a quick-reference row (near `README.md:157`/`:180`) once the feature compiles
   and is tested.

## Pitfalls

**The reused-variable trap: `input_names` is not only a key ingredient.** `input_names`
(`get_input_names`) feeds two unrelated consumers in `cached.rs`: the key construction call at
`cached.rs:689`, and the argument list forwarded to `_no_cache` at `cached.rs:1024`/`1032`
(`once.rs:811`/`837` and the `#[concurrent_cached]` equivalent do the same with their own
`input_names`). Filtering `input_names` in place to drop skipped parameters, so the key omits
them, would *also* drop them from the `_no_cache(#(#input_names),*)` call - the companion
function still declares the full parameter list (its signature is untouched), so this produces a
wrong-arity call (E0061), or worse, if two parameters happen to share a type, a silently wrong
argument gets forwarded instead of the one dropped. `input_tys` is safe to filter in place - it
has exactly one consumer, the `make_cache_key_type` call - but `input_names` needs a second,
separately-filtered variable used only for the key.

**Stripping late leaves a copy behind.** If the strip pass runs after `inputs.clone()`
(`cached.rs:250`) rather than before it, `inputs` retains the un-stripped `PatType`s, and
anything downstream that quotes `inputs` (or a per-parameter clone taken from it) back into
generated code re-emits `#[cached_skip]` on a real parameter list. Since attribute macros have no
`attributes(...)` registration (see Desired work), rustc will then reject the *expanded* code -
not the original user code - with an error about an attribute macro not being usable on a
function parameter, at a span the user did not write and cannot map back to their source. Grep
every place a file reads `signature.clone()` or `inputs`/`input_tys.clone()`-style before landing
the strip call, not just the three raw-clone sites in the table above; the wrapper function itself
(`cached.rs:1441`, `concurrent_cached.rs:1499`/`1520`, `once.rs:1032`) is built from
`signature_no_muts`, which is `get_mut_signature(signature)` - if `get_mut_signature` receives an
unstripped `signature`, the *public* function the user calls keeps the attribute on its own
parameter list, which is the most visible way this could half-ship.

**`key`/`convert` must reject `#[cached_skip]`, not ignore it.** The `(Some(key_str),
Some(convert_expr), _)` and `(None, Some(convert_expr), Some(_))` arms of `make_cache_key_type`
(`helpers.rs:530-545`, `:546-550`) never look at `input_tys`/`input_names` - the key type and
conversion are entirely user-written. A `#[cached_skip]`-marked parameter under `key`/`convert`
would silently do nothing: the parameter is not part of the generated key regardless, and if the
user still expects `key`/`convert` to have adapted, they may believe skip is broken rather than
inapplicable. This must be a hard error at macro-expansion time (checked once the skip mask and
`args.key`/`args.convert` are both known, before `make_cache_key_type` is called), not a
documentation note, and it must be spanned at the `#[cached_skip]` attribute (per the 0043b rule:
span at the offending token, not the function name) so the caret lands on the parameter, not on
`fn_ident`.

**`#[once]` must reject `#[cached_skip]` unconditionally.** `once.rs` has no key at all
(confirmed: no `get_input_types` call, no `make_cache_key_type` call anywhere in the file); a
skip marker there has nothing to exclude from. Follow the existing `key`/`convert`-on-`#[once]`
rejection at `once.rs:392-409` exactly (same file, same shape of check), and place the new check
next to it so a reader sees all three "`#[once]` has no per-call key" rejections together.

**Generics: do not let the marker leak into the `signature.generics` guard's reasoning.** The
generic-function guard (`cached.rs:348-364`, `concurrent_cached.rs:325-341`) inspects
`signature.generics`, which is untouched by parameter-attribute stripping. A generic function
with a `#[cached_skip]`-marked type-parameter argument is still rejected by this guard exactly as
before (unless `key`+`convert` is also given, in which case `#[cached_skip]` is separately
rejected by the previous pitfall). Do not special-case generics for `#[cached_skip]`; there is
nothing to special-case.

**Position alignment.** The skip mask must be built with the same receiver-skipping filter as
`get_input_names`/`get_input_types` (`helpers.rs:636-647`, `:660-669`), over the *same* `inputs`
snapshot, or index `i` in the mask will not correspond to index `i` in `input_tys`/`input_names`
at the `helpers.rs:563` zip. Building the mask from a different pass over `signature.inputs`
(which still includes the receiver) than the one that produced `input_tys`/`input_names` (which
does not) is an easy way to introduce an off-by-one that only shows up on methods.

## Verification

UI/compile-fail goldens live in `tests/ui/` (`/home/james/dev/cached/tests/ui/`), one `.rs` +
`.stderr` pair per case, registered in a `trybuild::TestCases` runner - `tests/v3_macros_ui.rs`
is the primary registrar (see `tests/proc_macro_v3_ui.rs` for a smaller, topic-scoped example of
the same pattern). New hard errors need new golden pairs there, following 0043's precedent of
pinning both the message and the caret position. New cases to add:

- `#[cached_skip]` + `key`/`convert` together on `#[cached]` (and the `#[concurrent_cached]`
  mirror) - error spanned at the `#[cached_skip]` attribute, not the function name.
- `#[cached_skip]` on an `#[once]`-decorated parameter - error spanned at the `#[cached_skip]`
  attribute, following the `once.rs:392-409` message shape.
- (Optional, only if the implementation ends up needing it) `#[cached_skip]` on a receiver
  (`self`) parameter - decide whether this is a no-op (receiver is never keyed anyway, per
  `get_input_names`) or a rejected redundant marker, and pin whichever is chosen with a golden.

Regenerate stderr goldens with `TRYBUILD=overwrite cargo test --features "proc_macro,time_stores"`
per the existing 0043 convention, then inspect the diff by hand before committing it.

Runtime coverage (regular `#[test]`s, not UI goldens), one behavior per assertion:

- Two calls to a `#[cached]` function differing only in the `#[cached_skip]`-marked argument hit
  the same cache entry (assert on a call counter via a companion static, the pattern already used
  by the crate's other cache-hit tests) - proves the parameter is genuinely absent from the key,
  not merely unused in a hand-written `convert`.
- The generated `_no_cache` (and `_prime_cache`, and the `#[concurrent_cached]` inner/companion
  equivalents) still take the *full* original parameter list, including the skipped one, and a
  direct call to `_no_cache` with two different values for the skipped argument produces two
  different results - proves the companions were not accidentally arity-reduced by the key
  filtering (the exact failure mode flagged in Pitfalls).
- `#[cached_skip]` on a `&T`/`&mut T`/`Option<&T>` parameter (the ToOwned-conversion branch at
  `helpers.rs:564-576`) is skipped correctly and does not attempt to run that conversion at all -
  a type with no `ToOwned` impl in the skipped position should still compile.
- The mut-stripping and skip-stripping compose: `#[cached_skip] mut x: T` produces a public
  wrapper parameter with neither `mut` nor the marker, and an inner/`_no_cache` parameter with
  `mut` but not the marker (mirroring how `get_mut_signature` already treats plain `mut` today).

## Notes

- Targeted for 3.2, not 3.1 (per the handoff brief this record accompanies).
- Answers issue #188.
- Related: 0043 (error span precision - the new `#[cached_skip]` rejections should follow its
  attribute-span rule from the start, not need a follow-up pass to fix their spans).
- Not in scope: renaming or aliasing the key contribution of a kept parameter, or a
  `#[cached_skip]`-adjacent way to reorder key fields. Issue #188 only asks for exclusion.
