---
name: release
description: Bump versions across all Cargo.toml files and update CHANGELOG.md for a new release. Use when asked to "cut a release", "bump the version", "prepare a release", or "release X.Y.Z". Takes a version string as the argument (e.g. `/release 1.2.0`).
---

# Release

Prepare a new release by bumping versions and updating the changelog.

## Input

The argument is the new version string, e.g. `1.2.0`. If no argument is given, ask the user which version to release.

## Steps

### 1. Determine which crates to bump

This repo has three crates:
- `cached` — always bumped
- `cached_proc_macro` — bump if this PR/branch touched `cached_proc_macro/`
- `cached_proc_macro_types` — bump only if this PR/branch touched `cached_proc_macro_types/`

Check `git diff origin/master --name-only` to determine which crate directories were modified. When in doubt, bump `cached` and `cached_proc_macro` together (the common case); `cached_proc_macro_types` rarely changes.

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

### 4. Regenerate README

```bash
cargo readme --no-indent-headings > README.md
```

### 5. Verify

```bash
cargo check --no-default-features --features "proc_macro,time_stores"
```

Fix any compilation errors before proceeding.

### 6. Commit or amend

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

### 7. Report

Tell the user:
- Which crates were bumped and to what version
- Whether `cached_proc_macro_types` was left unchanged and why
- That README was regenerated
- The resulting commit SHA
