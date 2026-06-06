---
name: pr-review
description: Targeted, read-only review of a PR or checked-out branch. Acquires the diff (a PR number, or the current branch vs origin/master), spawns an independent code-review sub-agent and a library-consumer sub-agent in parallel, then aggregates their findings into a single report with severity and a valid / already-fixed / invalid verdict for each. Read-only — it does not edit files, commit, push, or touch the GitHub PR conversation. The review sub-agents default to Sonnet but can be overridden per run (e.g. to opus). Use when asked to "review this PR", "review the branch", "what's wrong with this diff", "do a code review", or "review with opus". For the full review → fix → push → resolve loop, use `pr-cycle` (which delegates its review step here).
allowed-tools: Bash, Read, Agent
---

# PR Review

Produce a fresh, read-only review of a PR or a checked-out branch and report the
findings. This is the "review" half of the PR workflow, extracted so it can be run
on its own. The orchestrator skill `pr-cycle` calls this skill to obtain its local
findings, then goes on to address, push, and resolve them.

## Scope — what this does and does not do

**Does:** acquire the diff, spawn the two read-only review sub-agents, evaluate
their findings, and report them with severity and a verdict.

**Does NOT:** edit files, run `make ci`, regenerate the README, commit, or push; and
it does **not** interact with the GitHub PR conversation — it does not read existing
PR comments/threads, resolve or minimize them, edit the PR body, or re-request
Copilot review. Those belong to `pr-cycle`. This skill only generates a fresh
agent-based review of the code itself.

This skill is purely advisory: its output is a findings report for a human (or for
`pr-cycle`) to act on. It applies no changes.

## Model tiers

| Tier | What | Step | Model |
|------|------|------|-------|
| 1 — cheap delegation | Read-only review sub-agents | 2 | Sonnet (pinned in agent def; overridable per-run, e.g. to opus — see [Input](#input)) |
| 2 — judgment core | Classify findings into valid / already-fixed / invalid | 3, 4 | session model (use Opus for the verdict pass) |

## Input

A target and an optional review-agent model override, in any order.

- **Target**: either a **PR number**, or **nothing** (review the current checked-out
  branch). If a PR number is omitted you may infer one from the current branch with
  `gh pr view --json number` (run with the sandbox disabled — see below), but a PR is
  **not required**: a plain checked-out branch is reviewed by diffing against
  `origin/master`.
- **Review-agent model**: the model used by the two sub-agents (`pr-code-reviewer`,
  `pr-consumer-reviewer`) **defaults to `sonnet`**, but can be overridden. If the input
  names a model (e.g. "review with opus", "opus reviewers", "model=opus"), pass that
  model to the Agent tool's `model` parameter when spawning **both** sub-agents in
  step 2. With no override, omit `model` so each agent uses its pinned Sonnet default.

Announce the resolved target and review-agent model at the start — e.g. "Reviewing
the current branch with **opus** reviewers" or "Reviewing PR #264 with Sonnet
reviewers" — before spawning anything.

## Steps

### 1. Acquire the diff

The diff is `git diff origin/master`, which works for any checked-out branch whether
or not it has a PR:

```bash
git diff origin/master
```

If you are targeting a specific PR, the `pr-cycle` helper prints the identical diff
and is equivalent:

```bash
.agents/skills/pr-cycle/pr.py PR_NUMBER diff
```

Capture the full diff text — it is fed verbatim to both sub-agents.

### 2. Spawn two independent sub-agents in parallel

**Agent A — code reviewer**: Spawn with the `pr-code-reviewer` agent type. Prompt must
include:
- The PR number (or branch name, if there is no PR)
- The full diff (from step 1)

**Agent B — library consumer**: Spawn with the `pr-consumer-reviewer` agent type. Prompt
must include:
- The PR number (or branch name)
- The full diff
- The current `src/lib.rs` doc comments and `README.md` (or relevant excerpts covering
  the changed APIs)

Both agents are read-only (no Edit/Write tools) and carry their full rubrics in their
agent definitions — do not re-specify the rubric in the prompt.

**Model override:** if the input requested a review-agent model (see [Input](#input)),
pass it to the Agent tool's `model` parameter on **both** spawns (e.g. `model: "opus"`).
With no override, omit `model` so each agent uses its pinned Sonnet default.

Launch both agents in parallel. Wait for both to complete before proceeding.

### 3. Evaluate all findings

Collect both sub-agent reports. For each finding, assign a verdict and explain your
reasoning:

- **Valid** — the concern is real and the code should change.
- **Already fixed** — the concern was valid in principle but the current code already
  handles it (the reviewer was working from a partial view).
- **Invalid** — the finding is incorrect or environment-specific (e.g. a rustc version
  mismatch on trybuild golden files, or a "missing" feature gate that is actually
  present).

This verdict pass is the judgment core; run it on the session model (use Opus). Do not
soften or pad — an invalid finding called valid sends `pr-cycle` (or a human) chasing a
non-issue.

### 4. Report

Present a single consolidated report:

- The target reviewed (PR number or branch name) and the review-agent model used.
- **Code-reviewer findings**: total count, broken down by severity (high / medium / low),
  and by verdict (valid / already-fixed / invalid).
- **Consumer-reviewer findings**: the same breakdown.
- For each **valid** finding: a one-line summary, the `file:line` (or area), and why it
  matters — enough that `pr-cycle` or a human can act on it without re-reading the agent
  output.
- For each **invalid** or **already-fixed** finding: a one-line note on why it was ruled
  so.
- A closing one-line verdict: is the branch/PR clean, or are there valid findings to
  address (and how many high/medium)?

Do not apply any fix. If the caller wants the findings addressed and pushed, that is
`pr-cycle`'s job.
