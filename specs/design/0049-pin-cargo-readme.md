# 0049 - Pin the cargo-readme version CI installs

Status: Implemented

## Current state

- `.github/workflows/build.yml:85` runs `cargo install cargo-readme` with no version pin,
  immediately before `make ci` (`build.yml:87`). `make ci` runs `make check` (`Makefile:112,358`),
  which runs `check/readme` (`Makefile:358,367-371`):
  ```
  check/readme:
      $(README_CC) $(README_CCFLAGS) > _tmp_readme.md
      cmp README.md _tmp_readme.md
  ```
  `README_CC` is `cargo readme` (`Makefile:81`). Every CI run installs whatever `cargo-readme`
  version is newest on crates.io at that moment, with nothing in the repo recording what version
  last generated the committed `README.md`.
- Local dev docs install it the same unpinned way: `AGENTS.md:262` and
  `CONTRIBUTING.md:10-11` both say `cargo install cargo-readme` with no version.
- This has already broken CI once with no source change. `cargo-readme` 3.4.0 emits plain `rust`
  code fences and drops the hidden `# pub fn main` lines that 3.3.3 kept, so `check/readme` failed
  against the 3.3.3-generated `README.md`. Fixed by regenerating in commit `f639fb9` ("docs:
  regenerate README for cargo-readme 3.4.0"), which touched only `README.md` (6 insertions, 9
  deletions) with no doc-comment change in `src/lib.rs`.
- Locally installed and latest-on-crates.io `cargo-readme` is currently 3.4.0 (verified: `cargo
  readme --version` -> `cargo-readme-readme 3.4.0`; `cargo search cargo-readme` -> `cargo-readme =
  "3.4.0"`). `README.md` is currently generated with 3.4.0 output, so pinning to 3.4.0 now would
  not require a regeneration.
- The prerequisite noted in an earlier follow-up (do this after PR #299 merges) is satisfied: #299
  merged as `6b2d563` and is present in `master`'s history well before `HEAD`.

## Desired work

- Pin the CI install at `.github/workflows/build.yml:85`:
  ```
  - run: cargo install cargo-readme --version 3.4.0 --locked
  ```
  `--version` pins `cargo-readme` itself (what actually matters here); `--locked` additionally
  makes the install reproducible against `cargo-readme`'s own `Cargo.lock` rather than
  re-resolving its transitive deps on every install, which is a smaller but free win.
- Update the two local-dev references to match, so a contributor who copies the documented command
  installs the same version CI uses instead of silently drifting onto a newer one:
  `AGENTS.md:262` and `CONTRIBUTING.md:10-11`.
- Document the pinned version next to the `check/readme` target so it does not require reading the
  CI workflow to discover: a comment above `Makefile:367` (`check/readme:`) naming the version CI
  installs. This is documentation only — `README_CC` (`Makefile:81`) has no version awareness and
  will run whatever `cargo readme` is on `PATH` — so the comment does not enforce the pin by
  itself; enforcement lives in the CI install step and in whatever command the contributor actually
  runs locally.

## Notes

- Bumping the pin deliberately later: install the new version locally (`cargo install cargo-readme
  --version <X.Y.Z> --locked`), run `make docs` to regenerate `README.md`, eyeball the diff (as
  `f639fb9` did for the fence-style change) to confirm it is a legitimate formatting difference and
  not a botched generation, then commit the regenerated `README.md` together with the version bump
  in `build.yml`, `AGENTS.md`, `CONTRIBUTING.md`, and the `Makefile` comment in one commit so
  `check/readme` never observes a version mismatch between what generated the committed file and
  what CI installs.
- Caching the installed `cargo-readme` binary in CI (e.g. via `actions/cache` keyed on the pinned
  version, alongside the existing cargo/target dir caches at `build.yml:61-83`) would avoid
  reinstalling it every run, but is a separate, optional speed optimization, not required for
  correctness here.

## Verification

- Could not verify: whether GitHub Actions' `cargo install` step has ever silently picked up a
  cached older binary instead of the latest (i.e., whether the break is guaranteed to recur on
  every future release or only on some). Treat "recurs on every future cargo-readme release" as
  the worst case to design against, not a confirmed guarantee about CI's install caching behavior.
