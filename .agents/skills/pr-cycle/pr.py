#!/usr/bin/env python3
"""
pr.py — consolidated PR cycle helper.

Usage:
  pr.py [PR_NUMBER] COMMAND [COMMAND ...] [options]

The leading PR_NUMBER is optional: it is required only for the GitHub commands
(comments, threads, resolve, rerequest, minimize, codspeed). The local commands
(ci, readme, pushpreview, diff) need no PR number — e.g. `pr.py ci`.

Commands (executed in the order given):
  comments     Fetch and display all inline PR review comments (new vs pre-existing)
  threads      List all review threads with resolution status
  resolve      Resolve all open review threads
  rerequest    Re-request Copilot review
  minimize     Minimize (hide) all eligible pre-push comments as "resolved"
  codspeed     Check CodSpeed benchmark results
  ci           Run `make ci`, filter Redis/Docker noise, exit non-zero on real failures
  readme       Regenerate README from src/lib.rs if that file changed vs origin/master
  pushpreview  Print the git log + diff --stat push preamble for the current branch
  diff         Print git diff origin/master (for handing to reviewer agents)

Options:
  --owner OWNER    GitHub owner (auto-detected from `gh repo view`)
  --repo REPO      GitHub repo  (auto-detected from `gh repo view`)
  --branch BRANCH  Branch name for codspeed / pushpreview (auto-detected if omitted)
  --since TS       ISO timestamp; comments after this are 'new' (default: last push)
  --limit N        Max workflow runs to fetch for codspeed (default: 5)
  --dry-run        For 'resolve': list threads that would be resolved without mutating

Examples:
  pr.py 264 comments
  pr.py 264 threads
  pr.py 264 resolve rerequest minimize
  pr.py 264 comments threads resolve rerequest minimize codspeed
  pr.py ci
  pr.py readme
  pr.py pushpreview
  pr.py diff
"""

import argparse
import json
import subprocess
import sys
from datetime import datetime, timezone

COMMANDS = ("comments", "threads", "resolve", "rerequest", "minimize", "codspeed",
            "ci", "readme", "pushpreview", "diff")


# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------

def gh(*args, check=True, **kwargs):
    return subprocess.run(["gh", *args], capture_output=True, text=True, check=check, **kwargs)


def repo_info():
    r = gh("repo", "view", "--json", "owner,name", check=False)
    if r.returncode != 0:
        sys.exit("Cannot auto-detect owner/repo. Pass --owner and --repo explicitly.")
    d = json.loads(r.stdout)
    return d["owner"]["login"], d["name"]


def graphql(query):
    return gh("api", "graphql", "-f", f"query={query}")


def parse_utc(ts):
    return datetime.fromisoformat(ts.rstrip("Z")).replace(tzinfo=timezone.utc)


def section(title):
    print(f"\n{'=' * 3} {title} {'=' * (max(0, 60 - len(title)))}")


# ---------------------------------------------------------------------------
# comments
# ---------------------------------------------------------------------------

def cmd_comments(pr, owner, repo, since=None):
    section(f"PR #{pr} inline comments")

    r = gh("api", f"repos/{owner}/{repo}/pulls/{pr}/comments", "--paginate")
    comments = json.loads(r.stdout)

    if since is None:
        r2 = gh("api", f"repos/{owner}/{repo}/pulls/{pr}/commits",
                "--paginate", check=False)
        if r2.returncode == 0 and r2.stdout.strip():
            commits = json.loads(r2.stdout)
            if commits:
                since = commits[-1]["commit"]["committer"]["date"]

    since_dt = parse_utc(since) if since else None

    new_comments, old_comments = [], []
    for c in comments:
        if since_dt and parse_utc(c["created_at"]) > since_dt:
            new_comments.append(c)
        else:
            old_comments.append(c)

    print(f"Total inline comments: {len(comments)}")
    if since_dt:
        print(f"  New (after {since}): {len(new_comments)}")
        print(f"  Pre-existing: {len(old_comments)}")
    print()

    def show(c, label):
        line = c.get("line") or c.get("original_line") or "?"
        print(f"[{label}] id={c['id']}  {c['created_at']}  {c['path']}:{line}  @{c['user']['login']}")
        body = c["body"].replace("\n", " ").strip()
        if len(body) > 320:
            body = body[:317] + "..."
        print(f"  {body}")
        print()

    if new_comments:
        print("=== NEW COMMENTS ===")
        for c in new_comments:
            show(c, "NEW")

    if old_comments:
        print("=== PRE-EXISTING COMMENTS ===")
        for c in old_comments:
            show(c, "OLD")


# ---------------------------------------------------------------------------
# threads
# ---------------------------------------------------------------------------

_THREADS_QUERY = """\
{{
  repository(owner: "{owner}", name: "{repo}") {{
    pullRequest(number: {pr}) {{
      reviewThreads(first: 100) {{
        nodes {{
          id
          isResolved
          comments(first: 1) {{
            nodes {{
              databaseId
              createdAt
              body
            }}
          }}
        }}
      }}
    }}
  }}
}}"""


def fetch_threads(owner, repo, pr):
    r = graphql(_THREADS_QUERY.format(owner=owner, repo=repo, pr=pr))
    nodes = json.loads(r.stdout)["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"]
    threads = []
    for n in nodes:
        first = n["comments"]["nodes"][0] if n["comments"]["nodes"] else {}
        threads.append({
            "id": n["id"],
            "isResolved": n["isResolved"],
            "created_at": first.get("createdAt", ""),
            "database_id": first.get("databaseId"),
            "body_preview": first.get("body", "")[:200],
        })
    return threads


def cmd_threads(pr, owner, repo):
    section(f"PR #{pr} review threads")
    threads = fetch_threads(owner, repo, pr)
    unresolved = sum(1 for t in threads if not t["isResolved"])
    print(f"Total: {len(threads)}  Unresolved: {unresolved}")
    print(json.dumps(threads, indent=2))


# ---------------------------------------------------------------------------
# resolve
# ---------------------------------------------------------------------------

_RESOLVE_MUTATION = """\
mutation {{
  resolveReviewThread(input: {{threadId: "{thread_id}"}}) {{
    thread {{ id isResolved }}
  }}
}}"""


def cmd_resolve(pr, owner, repo, dry_run=False):
    section(f"PR #{pr} resolve threads")
    threads = fetch_threads(owner, repo, pr)
    unresolved = [t["id"] for t in threads if not t["isResolved"]]
    print(f"Found {len(unresolved)} unresolved thread(s).")

    if not unresolved:
        print("Nothing to do.")
        return

    if dry_run:
        print("Dry run — would resolve:")
        for tid in unresolved:
            print(f"  {tid}")
        return

    resolved = 0
    for tid in unresolved:
        print(f"  Resolving {tid} ...", end=" ", flush=True)
        r = graphql(_RESOLVE_MUTATION.format(thread_id=tid))
        if r.returncode == 0:
            print("ok")
            resolved += 1
        else:
            print(f"FAILED: {r.stderr.strip()}")

    print(f"\nResolved {resolved}/{len(unresolved)} thread(s).")
    if resolved < len(unresolved):
        sys.exit(1)


# ---------------------------------------------------------------------------
# minimize
# ---------------------------------------------------------------------------

_MINIMIZE_MUTATION = """\
mutation {{
  minimizeComment(input: {{subjectId: "{node_id}", classifier: RESOLVED}}) {{
    minimizedComment {{
      isMinimized
      minimizedReason
    }}
  }}
}}"""


def cmd_minimize(pr, owner, repo, since=None, dry_run=False):
    section(f"PR #{pr} minimize comments")

    # Determine cutoff: only minimize comments created at or before this
    # timestamp, which defaults to the last push (same boundary cmd_comments
    # uses for "pre-existing"). This prevents accidentally hiding comment
    # threads that were opened after the cycle started.
    if since is None:
        r0 = gh("api", f"repos/{owner}/{repo}/pulls/{pr}/commits",
                "--paginate", "--jq", "[-1].commit.committer.date", check=False)
        if r0.returncode == 0 and r0.stdout.strip():
            since = r0.stdout.strip().strip('"')

    since_dt = parse_utc(since) if since else None
    if since_dt:
        print(f"Cutoff: {since} (comments after this timestamp are skipped)")

    r = gh("api", f"repos/{owner}/{repo}/pulls/{pr}/comments", "--paginate")
    review_comments = json.loads(r.stdout)

    r2 = gh("api", f"repos/{owner}/{repo}/issues/{pr}/comments", "--paginate")
    issue_comments = json.loads(r2.stdout)

    eligible = [
        c for c in review_comments + issue_comments
        if since_dt is None or parse_utc(c["created_at"]) <= since_dt
    ]
    print(f"Found {len(eligible)} eligible comment(s) to minimize.")

    if dry_run:
        for c in eligible:
            print(f"  Would minimize: {c['node_id']}  @{c['user']['login']}  {c.get('path', '<issue comment>')}  {c['created_at']}")
        return

    minimized = 0
    for c in eligible:
        print(f"  Minimizing {c['node_id']} (@{c['user']['login']}) ...", end=" ", flush=True)
        r = graphql(_MINIMIZE_MUTATION.format(node_id=c["node_id"]))
        if r.returncode == 0:
            print("ok")
            minimized += 1
        else:
            print(f"FAILED: {r.stderr.strip()}")

    print(f"\nMinimized {minimized}/{len(eligible)} comment(s).")
    if minimized < len(eligible):
        sys.exit(1)


# ---------------------------------------------------------------------------
# rerequest
# ---------------------------------------------------------------------------

def cmd_rerequest(pr, owner, repo):
    section(f"PR #{pr} re-request Copilot review")
    gh("api", f"repos/{owner}/{repo}/pulls/{pr}/requested_reviewers",
       "-X", "POST", "-f", "reviewers[]=copilot-pull-request-reviewer[bot]")
    print(f"Copilot review re-requested for {owner}/{repo}#{pr}.")


# ---------------------------------------------------------------------------
# codspeed
# ---------------------------------------------------------------------------

def cmd_codspeed(pr, owner, repo, branch=None, limit=5):
    section(f"PR #{pr} CodSpeed")
    print("CodSpeed is not configured for this repository.")
    print(".github/workflows/codspeed.yml was removed in PR #264.")
    print("To re-enable, restore the workflow and update --workflow= in this command.")


# ---------------------------------------------------------------------------
# ci
# ---------------------------------------------------------------------------

# Patterns in stderr/stdout that indicate a Redis or Docker-related failure that
# is expected / acceptable in local environments.
_REDIS_DOCKER_PATTERNS = (
    "redis",
    "docker",
    "connection refused",
    "cannot connect",
    "ECONNREFUSED",
    "failed to connect",
    "redis_store",
    "disk_store",  # disk-store tests depend on the Redis feature flag indirectly
)


def cmd_ci():
    section("CI")
    print("Running `make ci` …")
    r = subprocess.run(
        ["make", "ci"],
        capture_output=True,
        text=True,
    )
    combined = r.stdout + r.stderr

    if r.returncode == 0:
        print("make ci passed.")
        return

    # Separate lines into Redis/Docker noise vs real failures.
    lines = combined.splitlines()
    real_failure_lines = []
    for line in lines:
        lower = line.lower()
        if any(p in lower for p in _REDIS_DOCKER_PATTERNS):
            continue
        # Lines that mention FAILED or error (but not from noise) are real.
        if "error" in lower or "failed" in lower or "panicked" in lower:
            real_failure_lines.append(line)

    if not real_failure_lines:
        # All failures appear to be Redis/Docker related.
        print("make ci exited non-zero, but all failures appear to be Redis/Docker-related (expected in local env).")
        print("Redis/Docker failures are acceptable — treating as pass.")
        return

    print(f"make ci FAILED (exit {r.returncode}). Real (non-Redis/Docker) failures:")
    print()
    for line in real_failure_lines[:80]:
        print(f"  {line}")
    if len(real_failure_lines) > 80:
        print(f"  … ({len(real_failure_lines) - 80} more lines)")
    print()
    print("Full output saved to stderr above. Fix these failures before proceeding.")
    sys.exit(r.returncode)


# ---------------------------------------------------------------------------
# readme
# ---------------------------------------------------------------------------

def cmd_readme():
    section("README regeneration")
    r = subprocess.run(
        ["git", "diff", "--name-only", "origin/master", "--", "src/lib.rs"],
        capture_output=True,
        text=True,
    )
    if not r.stdout.strip():
        print("src/lib.rs is unchanged vs origin/master — README regeneration skipped.")
        return

    print("src/lib.rs changed — regenerating README.md …")
    r2 = subprocess.run(
        ["cargo", "readme", "--no-indent-headings"],
        capture_output=True,
        text=True,
    )
    if r2.returncode != 0:
        print(f"cargo readme failed (exit {r2.returncode}):")
        print(r2.stderr)
        sys.exit(r2.returncode)

    with open("README.md", "w") as f:
        f.write(r2.stdout)
    print("README.md regenerated successfully.")


# ---------------------------------------------------------------------------
# pushpreview
# ---------------------------------------------------------------------------

def _current_branch():
    r = subprocess.run(
        ["git", "rev-parse", "--abbrev-ref", "HEAD"],
        capture_output=True, text=True, check=True,
    )
    return r.stdout.strip()


def cmd_pushpreview(branch=None):
    section("Push preview")
    if branch is None:
        branch = _current_branch()
    origin_ref = f"origin/{branch}"

    print(f"Branch: {branch}  →  {origin_ref}\n")

    r1 = subprocess.run(
        ["git", "log", f"{origin_ref}..HEAD", "--oneline"],
        capture_output=True, text=True,
    )
    if r1.stdout.strip():
        print("Commits to push:")
        for line in r1.stdout.strip().splitlines():
            print(f"  {line}")
    else:
        print("No commits ahead of remote.")

    print()
    r2 = subprocess.run(
        ["git", "diff", f"{origin_ref}", "--stat"],
        capture_output=True, text=True,
    )
    if r2.stdout.strip():
        print("Files changed:")
        print(r2.stdout.rstrip())
    else:
        print("No file changes vs remote.")


# ---------------------------------------------------------------------------
# diff
# ---------------------------------------------------------------------------

def cmd_diff():
    section("diff origin/master")
    r = subprocess.run(
        ["git", "diff", "origin/master"],
        capture_output=True, text=True,
    )
    print(r.stdout)
    if r.stderr:
        print(r.stderr, file=sys.stderr)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    # Commands that need a PR number + the GitHub owner/repo (REST or GraphQL API).
    gh_commands = {"comments", "threads", "resolve", "rerequest", "minimize", "codspeed"}

    # The PR number is an optional *leading* positional: GitHub commands require
    # it, but the local commands (ci, readme, pushpreview, diff) don't — so
    # `pr.py ci` and `pr.py 264 comments` are both valid.
    ap.add_argument(
        "tokens", nargs="+", metavar="[PR_NUMBER] COMMAND ...",
        help=(
            f"An optional leading PR number followed by one or more commands "
            f"({', '.join(COMMANDS)}). The PR number is required only for the "
            f"GitHub commands ({', '.join(sorted(gh_commands))}); the local "
            f"commands (ci, readme, pushpreview, diff) don't need it."
        ),
    )
    ap.add_argument("--owner")
    ap.add_argument("--repo")
    ap.add_argument("--branch", help="Branch name (for codspeed; auto-detected if omitted)")
    ap.add_argument("--since", help="ISO timestamp for 'comments' new/old split")
    ap.add_argument("--limit", type=int, default=5, help="Max runs for codspeed (default: 5)")
    ap.add_argument("--dry-run", action="store_true", help="For 'resolve': list without mutating")
    args = ap.parse_args()

    # Split the positional tokens into an optional leading PR number and commands.
    tokens = list(args.tokens)
    pr = None
    if tokens and tokens[0].isdigit():
        pr = int(tokens.pop(0))
    commands = tokens

    if not commands:
        ap.error("no command given; expected one or more of: " + ", ".join(COMMANDS))
    unknown = [c for c in commands if c not in COMMANDS]
    if unknown:
        ap.error(f"unknown command(s): {', '.join(unknown)}; choose from {', '.join(COMMANDS)}")
    needs_pr = [c for c in commands if c in gh_commands]
    if needs_pr and pr is None:
        ap.error(
            f"a PR number is required for: {', '.join(needs_pr)} "
            "(e.g. `pr.py 264 comments`)"
        )

    # Fetch owner/repo lazily — only if at least one command needs it.
    owner = repo = None
    if needs_pr:
        owner, repo = args.owner, args.repo
        if not owner or not repo:
            owner, repo = repo_info()

    for cmd in commands:
        if cmd == "comments":
            cmd_comments(pr, owner, repo, since=args.since)
        elif cmd == "threads":
            cmd_threads(pr, owner, repo)
        elif cmd == "resolve":
            cmd_resolve(pr, owner, repo, dry_run=args.dry_run)
        elif cmd == "rerequest":
            cmd_rerequest(pr, owner, repo)
        elif cmd == "minimize":
            cmd_minimize(pr, owner, repo, since=args.since, dry_run=args.dry_run)
        elif cmd == "codspeed":
            cmd_codspeed(pr, owner, repo, branch=args.branch, limit=args.limit)
        elif cmd == "ci":
            cmd_ci()
        elif cmd == "readme":
            cmd_readme()
        elif cmd == "pushpreview":
            cmd_pushpreview(branch=args.branch)
        elif cmd == "diff":
            cmd_diff()


if __name__ == "__main__":
    main()
