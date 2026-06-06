---
name: consumer-experience-review
description: Review a library/package from the perspective of an external downstream consumer — build a throwaway crate/project that depends on it the way a real user would, exercise the public API, and surface gaps, inconsistencies, and inconveniences with compiler-verified evidence. Use when asked to "review the consumer/user experience", "find API gaps/inconsistencies", "evaluate the public API", "is this ready to publish/release", or before a 1.0 / major version bump. Especially valuable for breaking-change releases (the only non-breaking window to fix the public surface).
---

# Consumer-experience review

You are auditing a library as the people who will `cargo add` / `npm install` / `pip install` it — not as its author. Internal tests pass through the crate's own privacy and import context and therefore **cannot** see the gaps a real consumer hits (missing root re-exports, trait-import friction, method-name collisions, feature-flag dead-ends, error types you can't name). The only reliable instrument is a separate project that depends on the library **externally** and uses it the way a reasonable-but-not-omniscient user would.

## Core principle

**Write the code a competent user would write on their first try — reaching for conventional names and paths — and let it fail. The natural call that doesn't compile *is* the finding.** Do not pre-correct your consumer code with insider knowledge; that hides exactly what you're looking for. Evidence is a captured compiler error, not an assertion.

## Procedure

### 1. Map the public surface (don't trust memory — read it)

Enumerate what a consumer actually sees:
- **Crate-root re-exports** vs what's only reachable via deeper paths (`crate::stores::Foo`). Asymmetry here is the #1 source of findings.
- **Traits and every method signature** — look for two traits that define the **same method name** (collides when both are in scope; common with sync/async pairs), and for `&mut self` where a consumer would expect `&self`.
- **Constructors / builders** — is `Type::new()` returning `Type` or a builder? Is the fallible path `build()` or `try_build()`? Is it consistent across sibling types?
- **Error types** — can the error returned by a public function be **named via the same path the type came from**? (`cached::Foo` but only `cached::stores::FooBuildError` is a classic gap.)
- **Feature flags** — what's gated, and does a natural feature combination leave a type unreferenceable or a macro unusable?
- **Macros** — re-exported at the crate root (`use lib::thing;`) or only under a submodule? Ecosystem convention is root.

### 2. Scaffold a throwaway external consumer

- Put it in `$TMPDIR` (or another scratch path) — **never inside the repo, never in git**. Do not pollute the project's `examples/`, `tests/`, or working tree.
- Depend on the library **by path** with a realistic feature set:
  - Rust: `lib = { path = "/abs/path", features = [...] }`
  - npm: `npm link` / `file:` dependency. Python: `pip install -e`. Same idea — external module resolution, real package boundary.
- Exercise the breadth: each macro, direct store/type use, sync and async paths, builders, error handling, and at least one less-common feature combination.

### 3. Write naive-but-reasonable consumer code

Deliberately reach for conventions first:
- Import the macro from the crate root (`use lib::macro;`) before trying submodule paths.
- Bring the obvious traits into scope together (or glob `use lib::*;`) — exactly what a real user does.
- Name the error type returned by a builder/constructor in a `let _: Option<lib::TheError> = None;` or a function signature.
- Guess builder method names by convention (`max_size`, `capacity`, `with_*`) — a wrong guess that the compiler can't help disambiguate is a discoverability finding.

### 4. Compile, run, capture exact errors

`cargo build`/`run` (or the ecosystem equivalent). For every failure, record the **exact** diagnostic code and message (`error[E0034]: multiple applicable items`, `E0432: unresolved import`, `E0599`, etc.). That verbatim error is the evidence in your report.

### 5. Isolate each gap in a minimal probe

For each suspected issue, create a tiny separate binary/module that triggers **only** that issue with the smallest natural snippet. This (a) proves it's real and not a side effect, and (b) gives you a clean before/after to re-run once a fix is proposed/applied.

### 6. Severity — weight the release window

Rank findings, and explicitly factor in the version context:
- **Pre-1.0 / major bump:** a hard compile error on a *common* operation, or any public-surface inconsistency, is **high** — this is the only non-breaking window to fix the method/type surface. Say so.
- Post-1.0 minor: a breaking fix is itself a cost; lean toward additive fixes (new re-exports, `#[doc(alias)]`, deprecations) and documentation.
- Always separate: 🔴 blocks/forces ugly workarounds on common paths · 🟠 awkward but workable · 🟡 docs/discoverability.

### 7. Report — evidence first, fixes proposed not applied

For each finding: the natural code that failed → the exact compiler error → root cause (cite `file:line`) → concrete proposed fix (and whether it's breaking or additive). Recommend a subset to act on given the release window. **Do not implement fixes unless the user asks** — this skill produces a review. If asked to fix, re-run the isolated probes (step 5) afterward to prove the natural code now works, and run the project's full check (`make check`/tests/clippy/doctests, golden/snapshot drift).

### 8. Clean up

Remove or abandon the scratch consumer crate. Never leave it in the repo or stage it.

## Gap classes to actively check (the checklist that catches the real ones)

- **Root re-export gaps / asymmetry** — type at root but its builder/error only via `crate::sub::`; one sibling exports `*Builder` at root, another doesn't.
- **Trait method-name collisions** — two in-scope traits with the same method name (sync `foo` vs async `foo`) → `E0034`, forces UFCS. Fix: prefix/rename one (e.g. `async_`-prefix the async variant), settle it pre-1.0.
- **Unnameable error types** — `Type::build()` returns `Result<_, E>` but `E` isn't reachable from the path `Type` is.
- **Constructor/builder inconsistency** — `new()`→value vs `new()`→builder; `build` vs `try_build`; one shared error enum vs per-type ones. Often documentation-only at 1.0, but flag it.
- **Macro not at crate root** — every mainstream crate puts its attribute/derive macro at the root; submodule-only is friction and a migration cost.
- **Feature-flag dead-ends** — a plausible feature set where a needed type/trait/macro is unreferenceable.
- **`&mut self` for reads** — forces a lock/wrapper for shared use; note it even if intentional.
- **Discoverability** — natural names that don't exist and have no `#[doc(alias)]`; missing trait-import hints in errors.

## Cautions

- Testing from inside the crate (its own `tests/`, `examples/`, or a workspace member) is **not** a consumer test — it inherits the crate's import/privacy context and will miss the gaps. External path dependency is mandatory.
- Sandboxed builds may hit registry/network errors; this skill only needs the path-dependency and std/`tokio`-class deps. If a build fails for sandbox/network reasons (not a real API gap), retry with the sandbox disabled before concluding.
- A finding is the *user's natural code failing*, not "the API is unusable" — the API may work fine once you know the trick. The cost being measured is "knowing the trick."
- Generalize beyond Rust where relevant: npm (root export maps, `exports` field, dual ESM/CJS), Python (`__init__.py` re-exports, optional-extra imports) — same methodology, same gap classes.
