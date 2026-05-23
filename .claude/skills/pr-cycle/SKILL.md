---
name: pr-cycle
description: Full PR review cycle — fetch open review comments, spawn an independent code-review sub-agent and a library-consumer sub-agent, evaluate all findings, apply valid fixes, run CI, amend and push, resolve all threads, and re-request review from Copilot. Use when asked to "run the pr cycle", "address pr comments", "resolve comments and re-request review", or after pushing a new round of changes.
---

# PR Cycle

Run one full iteration of the review → fix → push → re-request loop.

## Input

Optional PR number. If omitted, infer it from the current branch using `gh pr view --json number`.

## Steps

### 1. Fetch open Copilot comments

Run outside the sandbox (GitHub API requires network):

```bash
gh api repos/OWNER/REPO/pulls/PR/comments --paginate
```

Filter to comments created after the last push (compare `created_at` to the last force-push timestamp). Print each with: ID, file:line, and body.

Also fetch unresolved review threads via GraphQL to get their node IDs for later resolution:

```bash
gh api graphql -f query='{ repository(owner:"OWNER", name:"REPO") { pullRequest(number:PR) { reviewThreads(first:50) { nodes { id isResolved comments(first:1) { nodes { databaseId } } } } } } }'
```

### 2. Spawn two independent sub-agents in parallel

**Agent A — code reviewer**: Spawn with the `code-reviewer` subagent type (or general-purpose if unavailable). Prompt must include:
- The PR number and branch name
- The full diff (`git diff origin/master`)
- Instructions to review for correctness, API design, test coverage, documentation accuracy, and Rust idiom adherence
- Instructions to flag any issues with severity (high / medium / low)
- Instructions NOT to fix anything — report only

**Agent B — library consumer**: Spawn a general-purpose agent in a read-only research role. Prompt must include:
- The PR number and branch name
- The full diff (`git diff origin/master`)
- The current `src/lib.rs` doc comments and `README.md` (or the relevant excerpts covering the changed APIs)
- Instructions to evaluate the changes **solely from the perspective of a downstream crate author** who is adding or upgrading `cached` as a dependency — not as a reviewer of the implementation
- Specifically ask it to assess:
  - Is the public API surface intuitive? Are names, method signatures, and trait bounds what a user would expect?
  - Are the docs sufficient to use the feature without reading the source? Are there gaps, ambiguities, or missing examples?
  - Are there footguns — easy mistakes a user could make that aren't warned about in the docs?
  - Does the feature compose naturally with existing `cached` features (e.g., `result`, `option`, `result_fallback`, `sync_writes`)?
  - Are error messages from compile-fail cases clear enough for a user to self-diagnose?
- Instructions to flag any issues with severity (high / medium / low)
- Instructions NOT to fix anything — report only

Launch both agents in parallel. Wait for both to complete before proceeding.

### 3. Evaluate all findings

Present all three sets of findings (Copilot comments + code-reviewer + consumer reviewer) together. For each finding:
- **Valid**: the concern is real and the code should change
- **Invalid**: the finding is incorrect, already addressed, or environment-specific (e.g. rustc version mismatch on trybuild golden files)

Explain your reasoning for each verdict. Do not apply any fix silently — call out what you are doing and why.

### 4. Apply fixes for valid findings

For each valid finding, make the minimal correct fix. Common fix types for this repo:
- Documentation/comment updates in `src/stores/`, `src/lib.rs`, or `cached_proc_macro/src/lib.rs`
- Test additions or corrections in `tests/cached.rs`
- Trybuild golden file regeneration: `TRYBUILD=overwrite cargo test --no-default-features --features "proc_macro,time_stores" compile_fail_macro_arg_validation`
- Macro code changes in `cached_proc_macro/src/`

### 5. Run CI

```bash
make ci
```

Fix any errors. Redis and Docker failures are expected in local environments and can be ignored. All other failures must be resolved.

If trybuild golden files drift, regenerate them:
```bash
TRYBUILD=overwrite cargo test --no-default-features --features "proc_macro,time_stores" compile_fail_macro_arg_validation
```

### 6. Regenerate README if `src/lib.rs` changed

```bash
cargo readme --no-indent-headings > README.md
```

### 7. Amend and push

```bash
git add -p   # stage only changed files explicitly
git commit --amend --no-edit
git push --force-with-lease origin BRANCH
```

Do not add a `Co-Authored-By` line.

### 8. Resolve all open threads

For each unresolved thread node ID collected in step 1:

```bash
gh api graphql -f query='mutation { resolveReviewThread(input: {threadId: "THREAD_ID"}) { thread { id isResolved } } }'
```

Run outside the sandbox. Do not leave replies — resolve silently.

### 9. Re-request Copilot review

```bash
gh api repos/OWNER/REPO/pulls/PR/requested_reviewers \
  -X POST -f 'reviewers[]=copilot-pull-request-reviewer[bot]'
```

### 10. Report

- How many Copilot comments were found, how many were valid, how many were fixed
- How many code-reviewer findings were found, how many were valid, how many were fixed
- How many consumer-reviewer findings were found, how many were valid, how many were fixed
- Which findings were ruled invalid and why
- Confirm threads resolved and Copilot re-requested
- The resulting commit SHA and push status
