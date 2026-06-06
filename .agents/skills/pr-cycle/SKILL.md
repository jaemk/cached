---
name: pr-cycle
description: PR review-and-update cycle — the orchestrator that takes a PR from review to resolved. It runs a local review (by delegating to the `pr-review` skill), fetches open GitHub review comments, evaluates all findings, applies valid fixes, runs CI, commits, pushes, resolves threads, and re-requests Copilot review. Supports three modes — `full` (default, everything), `local` (only the local-review-and-fix loop; no GitHub PR-conversation reads or mutations), and `remote` (only the GitHub PR feedback loop; no local review). The review sub-agents default to Sonnet but can be overridden per run (e.g. to opus). Use when asked to "run the pr cycle", "address pr comments", "resolve comments and re-request review", "review and fix the branch", "run the remote pr cycle", or after pushing a new round of changes. For a read-only review with no fixes/push, use `pr-review` instead.
allowed-tools: Bash, Read, Edit, Write, Agent
---

# PR Cycle

Run one full iteration of the review → fix → push → re-request loop. This is the
**orchestrator**: it produces findings, addresses them with fixes, and updates the PR
(commit, push, resolve threads, re-request review).

The **review** half is owned by the separate `pr-review` skill, which spawns the two
read-only review sub-agents and reports their findings. `pr-cycle` does not duplicate
that logic — step 2 below delegates to `pr-review`. Reach for `pr-review` directly when
you only want a read-only review (no fixes, no push, no GitHub-conversation changes);
reach for `pr-cycle` when you want those findings actually addressed and the PR updated.

The helper script is `.agents/skills/pr-cycle/pr.py`. Run it outside the sandbox (GitHub API requires network). Multiple commands can be passed in one call so only one permission prompt is needed:

```bash
.agents/skills/pr-cycle/pr.py [PR_NUMBER] COMMAND [COMMAND ...]
```

The leading `PR_NUMBER` is optional — it is required only for the GitHub commands (`comments`, `threads`, `resolve`, `rerequest`, `minimize`, `codspeed`). The local commands (`ci`, `readme`, `pushpreview`, `diff`) take no PR number, so e.g. `pr.py ci` works directly (no placeholder needed).

Available commands: `comments`, `threads`, `resolve`, `rerequest`, `minimize`,
`codspeed`, `ci`, `readme`, `pushpreview`, `diff`.

## Modes

This skill runs in one of three modes. The mode is taken from the input (see [Input](#input)):

- **`full`** (default) — the complete review → fix → push → resolve → re-request loop. Runs every step below.
- **`local`** — only the **local review and fix loop**. Runs the `pr-review` skill (the local code-review and consumer sub-agents), evaluates their findings, applies fixes, runs CI, regenerates the README, and commits/pushes. Does **not** interact with the PR conversation on GitHub: it does not read PR comments or threads, does not edit the PR body, and does not resolve, minimize/hide, or re-request Copilot review.
- **`remote`** — only the **GitHub PR feedback loop**. Fetches and evaluates open PR comments/threads, applies fixes, runs CI, regenerates the README, commits/pushes, then resolves threads, minimizes prior comments, re-requests Copilot review, and audits the PR body. Does **not** run the local review (`pr-review`).

Both `local` and `remote` still commit and push the fixes they make (a code change has to land to be useful), and both follow the push protocol (show the push preamble before pushing). "Does not interact with GitHub" for `local` means the PR-conversation operations — comment/thread reads and mutations, Copilot re-request, PR-body edits — not the `git push` itself.

Step-by-step applicability (✓ = runs in that mode):

| Step | What | `full` | `local` | `remote` |
|------|------|:------:|:-------:|:--------:|
| 1 | Fetch open comments + threads | ✓ | | ✓ |
| 2 | Run local review (`pr-review`) | ✓ | ✓ | |
| 3 | Evaluate findings | ✓ | ✓ (agents only) | ✓ (PR comments only) |
| 4 | Apply fixes | ✓ | ✓ | ✓ |
| 5 | Run CI | ✓ | ✓ | ✓ |
| 6 | Regenerate README | ✓ | ✓ | ✓ |
| 7 | Sync audit (CHANGELOG / commit msg) | ✓ | ✓ | ✓ |
| 7c | Sync audit — PR body edit | ✓ | | ✓ |
| 8 | Commit + push | ✓ | ✓ | ✓ |
| 9 | Resolve threads + re-request Copilot | ✓ | | ✓ |
| 10 | Minimize prior comments | ✓ | | ✓ |
| 11 | Report | ✓ | ✓ | ✓ |

When a step is not applicable to the active mode, skip it entirely — do not run its `pr.py` command or `gh` call. In `local` mode you must not invoke any of `comments`, `threads`, `resolve`, `rerequest`, `minimize`, or `gh pr edit`/`gh pr view --json body`.

## Model tiers

This skill is designed to keep expensive Opus reasoning concentrated in the
judgment core and push everything else to cheaper models or to no model at all.

| Tier | What | Steps | Model |
|------|------|-------|-------|
| 0 — mechanical | All GitHub API ops, `make ci`, README regen, push preamble | 1, 5, 6, 8(preamble), 9, 10 | script (`pr.py`) |
| 1 — cheap delegation | Local review via `pr-review` (read-only sub-agents); mechanical/repetitive fix application | 2, 4b, 11 | Sonnet (pinned in agent def; review sub-agents overridable per-run, e.g. to opus — see [Input](#input)) |
| 2 — judgment core | Classify findings; write explicit fix specs + test assertions; sync audit | 3, 4a, 7 | Opus (session model) |

A Sonnet session can drive the whole cycle; only Tier-2 actually needs strong
reasoning, so consider switching the session to a cheaper model once the judgment
core is done.

## Input

Optional PR number, an optional mode keyword (`full`, `local`, or `remote`), and an optional review-agent model override, in any order.

- **Mode**: if one of `local` / `remote` is present in the input, use it; otherwise default to `full`. Phrasings map as: "local review" / "just the local reviewers" / "local pr cycle" → `local`; "remote" / "address the pr comments" / "resolve and re-request" / "remote pr cycle" → `remote`; anything else (or "run the pr cycle") → `full`.
- **Review-agent model**: the model used by the local review sub-agents (`pr-code-reviewer`, `pr-consumer-reviewer`) **defaults to `sonnet`**, but can be overridden. If the input names a model (e.g. "use opus for the reviewers", "review with opus", "opus reviewers", "model=opus"), pass that model through to the `pr-review` delegation in step 2 (which forwards it to the Agent tool's `model` parameter on both sub-agent spawns). Only the review sub-agents are affected — this does not change the session model or the model used by `pr-fix-implementer`. This override is only meaningful in modes that run the local review (`full`, `local`); ignore it in `remote` mode.
- **PR number**: if omitted, infer it from the current branch using `gh pr view --json number`. In `local` mode no PR number is needed at all — the `pr.py` wrapper commands used there (`ci`, `readme`, `pushpreview`, `diff`) take no PR argument, so call them directly (e.g. `pr.py ci`). This also means `local` mode works without `gh` installed.

Announce the resolved mode (and, when the reviewers will run, the review-agent model) at the start — e.g. "Running pr-cycle in **local** mode with **opus** reviewers" — before executing any step.

## Steps

### 1. Fetch ALL open comments and unresolved threads

**Modes: `full`, `remote`.** Skip entirely in `local` mode.

Run outside the sandbox:

```bash
.agents/skills/pr-cycle/pr.py PR_NUMBER comments threads
```

`comments` fetches all inline review comments, separates them into new (post-last-push) vs pre-existing unresolved, and prints each with its ID, `created_at`, file:line, author, and body. Record every comment for evaluation in step 3 — do not filter by timestamp.

`threads` lists all review thread node IDs with their resolution status. Unresolved threads will all be resolved in step 10.

### 2. Run the local review (delegate to `pr-review`)

**Modes: `full`, `local`.** Skip entirely in `remote` mode (no local review runs).

The review itself lives in the `pr-review` skill — do not re-implement it here. Run that
skill against this PR/branch to obtain the local findings, passing through the
review-agent model override from the Input if one was given. `pr-review` will:

- acquire the diff (`.agents/skills/pr-cycle/pr.py PR_NUMBER diff`, equivalent to
  `git diff origin/master`),
- spawn `pr-code-reviewer` and `pr-consumer-reviewer` in parallel (read-only, each
  carrying its own rubric; Sonnet by default, or the overridden model on **both**
  spawns), and
- return a consolidated findings report with severity and a per-finding verdict.

Carry `pr-review`'s findings forward into step 3, where they are evaluated alongside any
GitHub PR comments (in `full` mode). Do not act on fixes inside `pr-review` — it is
read-only; addressing findings is step 4 here.

### 3. Evaluate all findings

**Modes: all.** The set of findings depends on the mode:
- `full` — all open inline PR comments (from step 1) **plus** both sub-agent reports (from step 2).
- `local` — **only** the two sub-agent reports. Do not reference PR comments.
- `remote` — **only** the open inline PR comments. There are no sub-agent reports.

Present all in-scope findings together. For each finding:
- **Valid**: the concern is real and the code should change
- **Already fixed**: the concern was valid but the code has already been corrected (the comment is stale) — mark for resolution only
- **Invalid**: the finding is incorrect or environment-specific (e.g. rustc version mismatch on trybuild golden files)

Explain your reasoning for each verdict. Do not apply any fix silently — call out what you are doing and why.

### 4. Apply fixes for valid findings

**Modes: all.**

#### 4a. Write a fix spec for each valid finding (Opus / judgment core)

For each valid finding, produce an explicit fix spec before touching any file:

```
Finding: <one-line summary>
Target:  <file path>
Location: <file:line or unique surrounding snippet>
Change:  <exact new text, or precise add/remove/replace description — never "improve the wording">
Test:    <exact function name + exact assertions that would fail on unfixed code>
         OR "no test needed (doc-only)"
```

The spec must be precise enough for Sonnet to apply without judgment calls.
Test design always stays here — this repo requires tests that fail on the unfixed
code and pass on the fixed code; a trivially-passing test defeats the rule.

Common fix types:
- Documentation/comment updates: `src/stores/`, `src/lib.rs`, `cached_proc_macro/src/lib.rs`
- Test additions/corrections: `tests/cached.rs`
- Trybuild golden file regeneration: `TRYBUILD=overwrite cargo test --no-default-features --features "proc_macro,time_stores" compile_fail_macro_arg_validation`
- Macro code changes: `cached_proc_macro/src/`

#### 4b. Apply fixes (route per fix)

For each spec, choose the routing:

- **Delegate to `pr-fix-implementer` (Sonnet)** when the fix is **mechanical or
  repetitive across multiple files** (e.g. the same change in all six sharded stores,
  a doc-string pattern replicated across store modules). State "delegating because:
  <reason>". Spawn the `pr-fix-implementer` agent with the fix spec as the prompt.
- **Apply inline** when the fix is **a one-off, subtle, or logic/macro change**.
  For small one-off edits the spec-writing + verification round-trip costs more than
  editing directly. State "applying inline because: <reason>".

After all fixes are applied, verify with:

```bash
.agents/skills/pr-cycle/pr.py PR_NUMBER ci
```

Then spot-check: confirm that at least one of the newly added tests would fail if
its corresponding fix were reverted. (Read the test and reason through it; you do
not have to literally revert the fix.)

### 5. Run CI

**Modes: all.** Run outside the sandbox:

```bash
.agents/skills/pr-cycle/pr.py PR_NUMBER ci
```

This runs `make ci`, filters Redis/Docker noise, and exits non-zero only on real
failures. If it exits non-zero, fix the reported failures and re-run.

If trybuild golden files drift, regenerate them:
```bash
TRYBUILD=overwrite cargo test --no-default-features --features "proc_macro,time_stores" compile_fail_macro_arg_validation
```

### 6. Regenerate README if `src/lib.rs` changed

**Modes: all.** Run outside the sandbox:

```bash
.agents/skills/pr-cycle/pr.py PR_NUMBER readme
```

No-ops automatically when `src/lib.rs` is unchanged vs `origin/master`.

### 7. Sync audit — docs, commit message, and PR summary

**Modes: all** for parts (a), (b), (d). **Part (c) — PR-body edit — runs only in `full` and `remote`; skip it in `local`** (it reads and mutates the PR on GitHub).

Before staging anything, do a consistency check. The goal: every artifact that describes "what this PR does" must match the actual diff.

**a. Diff summary** — produce a concise internal summary of what the branch actually changes:

```bash
git diff origin/master --stat
git diff origin/master -- CHANGELOG.md
```

**b. CHANGELOG.md** — read the `[Unreleased]` section. For each bullet:
- Does it describe something that is actually in `git diff origin/master`? If a bullet refers to a feature or behavior that was removed, reverted, or renamed, update or remove it.
- Is anything significant in the diff that is NOT mentioned? Add it.
- Check accuracy of any named types, method signatures, attribute names, or feature gates — they must exactly match the code.

**c. PR description** (`full` / `remote` only — skip in `local`) — read the current PR body:

```bash
gh pr view PR_NUMBER --json body
```

Apply the same audit: every claim must match the diff. Pay special attention to:
- Named types or methods that were renamed or removed
- Feature/behavior claims that no longer apply (e.g. "replaces AtomicU64 across all stores" when only two stores were changed)
- Test counts that are now stale

Update the PR body if anything is inaccurate:

```bash
gh pr edit PR_NUMBER --body "..."
```

**d. Commit message** — draft a concise new commit message for the fixes made in this cycle. The message should describe only the newly applied changes, not the entire PR.

Do not make the sync audit a "wall of changes" — only fix what is actually wrong.

### 8. Create a new commit and push

**Modes: all** (only if fixes were applied this pass). Applies to `local` and `remote` too — fixes have to land to be useful.

```bash
git add -p   # stage only changed files explicitly
git commit -m "fix: address PR review feedback"
```

Before pushing, run outside the sandbox to show the push preamble:

```bash
.agents/skills/pr-cycle/pr.py PR_NUMBER pushpreview
```

Then add a one-sentence summary of what the push contains (e.g. "Pushing 1 commit:
doc fix for option+expires constraint and CHANGELOG update"). Then push:

```bash
git push origin BRANCH
```

Create a new commit for every PR-cycle pass that changes files. Do not amend previous commits and do not force push unless the user explicitly requests history rewriting.

Do not add a `Co-Authored-By` line.

### 9. Resolve all open threads and re-request Copilot review

**Modes: `full`, `remote`.** Skip entirely in `local` mode (no PR-conversation mutations, no Copilot re-request).

After the push, run outside the sandbox — combining both steps in one call:

```bash
.agents/skills/pr-cycle/pr.py PR_NUMBER resolve rerequest
```

`resolve` re-fetches all unresolved threads and resolves each one via GraphQL mutation. Goal: zero open threads after this step.

`rerequest` triggers a fresh Copilot review on the PR.

### 10. Minimize all comments from before this cycle

**Modes: `full`, `remote`.** Skip entirely in `local` mode.

After threads are resolved, hide all inline review comments and top-level PR comments so the PR conversation is clean. Run outside the sandbox:

```bash
.agents/skills/pr-cycle/pr.py PR_NUMBER minimize
```

This fetches all inline review comments and top-level PR comments from all authors and calls the GitHub `minimizeComment` GraphQL mutation with classifier `RESOLVED` on each one. Only comments created before the last push timestamp are minimized — comments posted after the push (i.e., responses to the new round of changes) are left visible. Use `--dry-run` first to preview which comments would be minimized.

### 11. Report

**Modes: all.** State the mode that ran, and report only the lines relevant to it.

- The mode that ran (`full` / `local` / `remote`).
- (`full` / `remote`) How many inline PR comments were found total; how many were new (post-last-push) vs. pre-existing unresolved; how many were valid/already-fixed/invalid; how many were fixed this cycle.
- (`full` / `local`) How many code-reviewer findings were found, how many were valid, how many were fixed.
- (`full` / `local`) How many consumer-reviewer findings were found, how many were valid, how many were fixed.
- Which findings were ruled invalid and why.
- Sync audit result: what was corrected in CHANGELOG, the new commit message, and — in `full` / `remote` — the PR description (or "all in sync").
- (`full` / `remote`) Confirm threads resolved (state total resolved count) and Copilot re-requested.
- The resulting new commit SHA and push status (or "no changes to commit this pass").
