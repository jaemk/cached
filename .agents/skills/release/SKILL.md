---
name: release
description: Prepare a release (bump versions across all Cargo.toml files, update CHANGELOG.md, refresh the migration guide, regenerate README, commit), or run a pre-release review. The `review` option kicks off a consistency review and an API-examination review to surface lingering inconsistencies and a categorized list of breaking and non-breaking improvements before you cut the release — advisory only, it changes nothing. Use when asked to "cut a release", "bump the version", "prepare a release", "release X.Y.Z", or "do a release review" / "review for release". Takes a version string (e.g. `/release 1.2.0`) or `review` as the argument.
---

# Release

Prepare a new release by bumping versions and updating the changelog — or, with the
`review` option, run a pre-release audit that surfaces inconsistencies and improvement
ideas without changing anything.

## Input

The argument selects the mode:

- **`review`** (or "release review", "review for release", "pre-release review") — run the
  **Release review** below. This is advisory only: it bumps nothing, edits no files, and
  makes no commit. Run it *before* cutting a release.
- **A version string** (e.g. `1.2.0`) — run the **Version bump** flow (the numbered steps).
  If no argument is given and the intent is clearly to cut a release, ask which version.

If the user asks for a release review and then to proceed, run the review first and the
version bump second.

## Release review

A pre-release audit. Run it **before** cutting a release — especially a major version,
since a major bump is the only window in which breaking API changes are acceptable. It
**does not** bump versions, edit the changelog/README, or commit anything; it produces a
report. It kicks off two complementary reviews, then synthesizes them.

### A. Consistency review

Audit the public surface for lingering inconsistencies across the **whole crate**, not
just the latest diff. Scope it to everything accumulated since the last released version.

Establish the baseline with plain, side-effect-free commands — run each separately rather
than nesting a `$(…)` command substitution, so each invocation can be statically verified
and auto-approved (a `$(git …)` substitution defeats static analysis and forces a prompt):

```bash
git describe --tags --abbrev=0    # last released tag
git tag --sort=-creatordate       # full tag list, newest first
git log --oneline -20             # recent commits — find the last "Release"/"release" commit
```

Read the last-released **version** off the result, then diff against it explicitly, e.g.:

```bash
git diff v1.1.0..HEAD --stat      # substitute the real last-released ref
```

**Tags in this repo are unreliable** — they have lagged the crate version before (e.g.
`git describe` returned `v0.8.0` while the crate was at `2.0.0`). Do not trust the tag
blindly. Cross-check it against the `[…]` version headings in `CHANGELOG.md` and the most
recent `Release`/`release` commit in the log; if they disagree, use the last *actually
released* commit (the one matching the newest released CHANGELOG section) as the diff
baseline, not the stale tag.

Check for:
- **Naming parity** — do sibling types/methods share vocabulary? (e.g. `max_size` vs
  `size`, `with_*` constructors, `try_with_*` fallible variants, `cache_*` method
  prefixes). Flag any odd-one-out.
- **Builder / constructor symmetry** — does every store with a builder expose the same
  setters where they make sense? Does each family (plain vs sharded, LRU vs TTL) have
  parallel constructors?
- **Trait-method parity** — do the sync / async / sharded variants of a trait expose the
  same method set with consistent signatures (`&self` vs `&mut self`)?
- **Feature-gate symmetry** — is each public item gated behind exactly the features it
  needs, and are paired items gated consistently?
- **Doc / CHANGELOG / migration-guide alignment** — do the docs, the `[Unreleased]`
  CHANGELOG section, and the migration guide accurately describe the shipped API (named
  types, signatures, attribute names, feature gates)?

Delegate the correctness/idiom sweep to a `pr-code-reviewer` sub-agent fed the full
release diff, and reason through the cross-type parity items yourself — they need a
whole-surface view the diff alone does not give.

### B. API examination review

Run the `consumer-experience-review` skill. It builds a throwaway external consumer that
depends on the crate the way a real user would and surfaces API gaps, naming
inconsistencies, trait-import friction, and feature-flag dead-ends with
**compiler-verified evidence**. This is the authoritative "what would a new user trip
over" pass.

### C. Synthesize the report

Merge both reviews into a single report. **Apply no change** — this mode is advisory.
Produce:

1. **Lingering inconsistencies** — a flat list of every inconsistency found, each with a
   `file:line` (or API path) and a one-line description.
2. **Recommended changes, split by impact:**
   - **Breaking** — changes that alter the public API (renames, signature changes, removed
     items, new required trait methods). For each: what to change, why it improves the
     library, and the migration cost. Mark these "land now or wait for the next major" — a
     major-version release is the only non-breaking window to make them.
   - **Non-breaking** — additive or internal changes (new constructors, doc fixes, new
     re-exports, deprecations that keep the old path working). For each: what to change and
     why.
3. **Recommendation** — is the library consistent enough to release as-is, or are there
   high-impact items that should land in this version first? Be explicit about which
   breaking items, if deferred, are stuck until the next major.

After presenting the report, ask whether to address findings first or proceed to the
version bump below.

## Version bump

These numbered steps are the default mode — run them when the argument is a version
string, or after a release review once the user opts to proceed.

### 1. Determine which crates to bump

This repo has three crates:
- `cached` — always bumped
- `cached_proc_macro` — bump if this PR/branch touched `cached_proc_macro/`
- `cached_proc_macro_types` — bump only if this PR/branch touched `cached_proc_macro_types/`

Run the helper script to detect which crates need bumping:

```bash
.agents/skills/release/detect-crates.sh
```

This outputs one crate name per line based on `git diff origin/master`. When in doubt, bump `cached` and `cached_proc_macro` together (the common case); `cached_proc_macro_types` rarely changes.

### 2. Update `Cargo.toml` versions

Files to update (only for crates being bumped):

**`Cargo.toml`** (the `cached` crate):
- `[package] version` → new version
- `[dependencies.cached_proc_macro] version` → new version (if bumping proc_macro)
- `[dependencies.cached_proc_macro_types] version` → new version (if bumping proc_macro_types)

**`cached_proc_macro/Cargo.toml`**:
- `[package] version` → new version (if bumping proc_macro)

**`cached_proc_macro_types/Cargo.toml`**:
- `[package] version` → new version (if bumping proc_macro_types)

Use precise string replacement — do not change dependency versions for third-party crates.

### 3. Update `CHANGELOG.md`

- Replace `## [Unreleased]` with `## [X.Y.Z / cached_proc_macro X.Y.Z]` (include only the crates being bumped in the heading — omit `cached_proc_macro_types` if it is not bumped)
- Add a fresh `## [Unreleased]` section above the new version heading
- The changelog must always have an `[Unreleased]` section at the top

### 4. Create or update migration guide

Every release requires a migration guide in `docs/migrations/` named `PREV-to-X.Y.Z.md`
(e.g. `1.1-to-1.2.md`). If there are no breaking changes, the guide still must exist and
must state there are no breaking changes.

Migration guides are written for **agent consumption**: terse, mechanical, grep-friendly.
Required sections:

- **Versions** header line
- **Breaking changes** — one subsection per change with Detection (what to grep/search for)
  and Action (exact code transformation). If none, write "None. This release is purely additive."
- **New APIs** — additive changes; note the feature gate if any
- **Required Cargo.toml change** — exact before/after snippet
- **VERIFY** — the `cargo build` / `cargo test` commands needed to confirm a successful migration,
  plus any expected new compile errors and their fixes

See existing guides in `docs/migrations/` for the established format.

If the guide for this version already exists (e.g. was drafted ahead of the release), review it
against the final diff and update any stale type names, method signatures, or behavior descriptions.

### 5. Regenerate README

```bash
cargo readme --no-indent-headings > README.md
```

### 6. Verify

```bash
cargo check --no-default-features --features "proc_macro,time_stores"
```

Fix any compilation errors before proceeding.

### 7. Commit or amend

If there is already a single commit ahead of master on this branch, amend it:
```bash
git add Cargo.toml cached_proc_macro/Cargo.toml cached_proc_macro_types/Cargo.toml CHANGELOG.md README.md
git commit --amend --no-edit
```

Otherwise create a new commit:
```bash
git commit -m "release: bump version to X.Y.Z"
```

Do not add a `Co-Authored-By` line.

### 8. Report

Tell the user:
- Which crates were bumped and to what version
- Whether `cached_proc_macro_types` was left unchanged and why
- That README was regenerated
- The resulting commit SHA
