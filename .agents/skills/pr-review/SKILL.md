---
name: pr-review
description: Targeted, read-only review of a PR or checked-out branch. Acquires the diff (a PR number, or the current branch vs origin/master), shards the changed material into appropriately sized, randomized chunks, and spawns multiple read-only code-review and library-consumer sub-agents in parallel (one per shard), then aggregates and de-duplicates their findings into a single report with severity and a valid / already-fixed / invalid verdict for each. Read-only — it does not edit files, commit, push, or touch the GitHub PR conversation. The review sub-agents default to Sonnet but can be overridden per run (e.g. to opus). Use when asked to "review this PR", "review the branch", "what's wrong with this diff", "do a code review", or "review with opus". For the full review → fix → push → resolve loop, use `pr-cycle` (which delegates its review step here).
allowed-tools: Bash, Read, Agent
---

# PR Review

Produce a fresh, read-only review of a PR or a checked-out branch and report the
findings. This is the "review" half of the PR workflow, extracted so it can be run
on its own. The orchestrator skill `pr-cycle` calls this skill to obtain its local
findings, then goes on to address, push, and resolve them.

## Scope — what this does and does not do

**Does:** acquire the diff, shard it into appropriately sized chunks, spawn the
read-only review sub-agents (one per shard, multiple of each type), evaluate and
de-duplicate their findings, and report them with severity and a verdict.

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
| 1 — cheap delegation | Read-only review sub-agents, one per shard | 3 | Sonnet (pinned in agent def; overridable per-run, e.g. to opus — see [Input](#input)) |
| 2 — judgment core | Shard the material; de-duplicate and classify findings into valid / already-fixed / invalid | 2, 4, 5 | session model (use Opus) |

## Input

A target and an optional review-agent model override, in any order.

- **Target**: either a **PR number**, or **nothing** (review the current checked-out
  branch). If a PR number is omitted you may infer one from the current branch with
  `gh pr view --json number` (run with the sandbox disabled — see below), but a PR is
  **not required**: a plain checked-out branch is reviewed by diffing against
  `origin/master`.
- **Review-agent model**: the model used by the two reviewer types (`pr-code-reviewer`,
  `pr-consumer-reviewer`) **defaults to `sonnet`**, but can be overridden. If the input
  names a model (e.g. "review with opus", "opus reviewers", "model=opus"), pass that
  model to the Agent tool's `model` parameter when spawning **all** shard sub-agents in
  step 3. With no override, omit `model` so each agent uses its pinned Sonnet default.
- **Shard sizing (optional)**: by default the orchestrator sizes shards automatically
  from the review-agent model — smaller shards for cheaper models, larger for stronger
  ones (see step 2). Override with an explicit target in the input if you want finer or
  coarser splitting, e.g. "shards of ~4 files", "one file per shard", or "single shard"
  (the latter restores the old whole-diff-per-reviewer behavior).

Announce the resolved target and review-agent model at the start — e.g. "Reviewing
the current branch with **opus** reviewers" or "Reviewing PR #264 with Sonnet
reviewers" — before spawning anything. After sharding (step 2), announce the shard
counts (e.g. "3 code shards, 2 consumer shards") before spawning the reviewers.

## Steps

### 1. Acquire the diff and build the review inventory

The diff is `git diff origin/master`, which works for any checked-out branch whether
or not it has a PR:

```bash
git diff origin/master
git diff origin/master --stat
```

If you are targeting a specific PR, the `pr-cycle` helper prints the identical diff
and is equivalent (`.agents/skills/pr-cycle/pr.py PR_NUMBER diff`).

From the changed-file list, build an inventory of **review units**. A unit is normally
one changed file, with one exception: keep **atomic couplings** together as a single
unit — a trybuild `tests/ui/<case>.rs` and its matching `<case>.stderr` (and any paired
source) must travel together, since reviewing one without the other is meaningless.

Tag each unit with the reviewer type(s) it needs:
- **Code-review set** — all code: `cached_proc_macro/src/`, `src/`, `tests/`, examples.
  Essentially every changed `.rs` file and golden file.
- **Consumer-review set** — public-facing surface only: `src/lib.rs`, the public APIs in
  `src/stores/`, `cached_proc_macro/src/lib.rs` (the macro attribute surface),
  `README.md`, `CHANGELOG.md`, `docs/migrations/`, and `examples/`. Internal macro
  plumbing and internal test helpers are not consumer-relevant.

A unit may belong to both sets (e.g. `src/lib.rs`).

### 2. Shard each set into appropriately sized, randomized chunks

The code set and the consumer set are sharded **independently**. Sharding has two jobs:
keep each shard small enough that the review model attends to every line, and vary the
grouping between rounds so repeated reviews surface different findings.

**a. Pick the target shard size from the review-agent model.** Cheaper models get
smaller shards; stronger models absorb more per shard without losing attention:

| Review model | Target per shard |
|--------------|------------------|
| sonnet (default) | ~600-900 changed diff lines, or ~4-6 units |
| opus | ~1500-2500 changed diff lines, or ~10-15 units |

An explicit shard-size override from the Input wins over this table. Use the
`--stat` line counts from step 1 for packing.

**b. Randomize the grouping, then pack.** Produce a fresh random ordering of the units
each run — `shuf` reseeds from the OS on every invocation, so each round yields a
different permutation:

```bash
git diff origin/master --name-only | shuf
```

Pack the shuffled unit list greedily: add units to the current shard until adding the
next would exceed the target size, then start a new shard. Because the order is
reshuffled every round, a given file lands with different neighbors each time — reviewers
see different cross-file context and surface different cross-cutting findings. Do **not**
re-sort the shuffled list into a tidy order; the randomness is the point. (Atomic
couplings from step 1 stay intact as one unit through the shuffle.)

This yields some number of code shards and consumer shards (each typically a handful).
Announce the counts before spawning.

### 3. Spawn one sub-agent per shard, in parallel

For each **code shard**, spawn a `pr-code-reviewer`. For each **consumer shard**, spawn a
`pr-consumer-reviewer`. Every agent's prompt must include:
- The target (PR number, or branch name if there is no PR)
- The explicit list of files in its shard
- An instruction to **scope its review to those files**: acquire its slice with
  `git diff origin/master -- <files...>` and Read those files in full for context, but
  report findings only on the assigned files.
- (consumer shards only) a pointer to the current `src/lib.rs` doc comments and
  `README.md` for the APIs its files touch.

Both agent types are read-only (no Edit/Write) and carry their full rubrics in their
agent definitions — do not re-specify the rubric in the prompt.

**Model override:** if the input requested a review-agent model (see [Input](#input)),
pass it to the Agent tool's `model` parameter on **every** spawn (e.g. `model: "opus"`).
With no override, omit `model` so each agent uses its pinned Sonnet default.

Spawn **all** shard agents in a single message so they run concurrently, and wait for all
to complete before proceeding. (Harness concurrency is capped; excess agents queue and
still complete.)

### 4. Evaluate all findings (de-duplicate across shards)

Collect every shard's report. Shards are disjoint, so most findings are unique, but a
cross-cutting issue can be reported by more than one shard (or by both a code and a
consumer reviewer) — **merge duplicates into one finding** before judging. For each
finding, assign a verdict and explain your reasoning:

- **Valid** — the concern is real and the code should change.
- **Already fixed** — the concern was valid in principle but the current code already
  handles it (the reviewer was working from a partial view).
- **Invalid** — the finding is incorrect or environment-specific (e.g. a rustc version
  mismatch on trybuild golden files, or a "missing" feature gate that is actually
  present).

This verdict pass is the judgment core; run it on the session model (use Opus). Do not
soften or pad — an invalid finding called valid sends `pr-cycle` (or a human) chasing a
non-issue.

### 5. Report

Present a single consolidated report:

- The target reviewed (PR number or branch name) and the review-agent model used.
- **Sharding**: how many code shards and consumer shards ran, and the target shard size
  used.
- **Code-reviewer findings**: total count (after de-dup), broken down by severity
  (high / medium / low), and by verdict (valid / already-fixed / invalid).
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
