#!/bin/bash

# Create a git tag and GitHub release for every workspace crate whose current
# version is not yet tagged on the remote.
#
# Idempotent: a crate whose tag already exists is skipped, so a run only tags
# the crates that were just published (their version bumped) and leaves
# unchanged crates alone. This means it transparently handles the subcrates
# (cached_proc_macro, cached_proc_macro_types) getting their own releases.
#
# Tag naming:
#   - root crate `cached`  -> vX.Y.Z          (bare, kept for back-compat)
#   - workspace subcrates  -> <crate-name>-vX.Y.Z
#
# Requires: git, jq, cargo, and the gh CLI authenticated (GH_TOKEN in CI).

set -euo pipefail

# The root crate keeps the bare `vX.Y.Z` tag; subcrates are namespaced by name.
ROOT_CRATE="cached"

# Use the bot identity for the annotated tags when running in CI; leave a local
# user's git config untouched otherwise. Scope the write to the repo-local config
# (`--local`) so it is explicit that only this checkout is affected, never the
# runner's global identity.
if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    git config --local user.name "github-actions[bot]"
    git config --local user.email "github-actions[bot]@users.noreply.github.com"
fi

tag_exists_on_remote() {
    git ls-remote --tags origin "refs/tags/$1" | grep -q "refs/tags/$1$"
}

release_exists() {
    gh release view "$1" >/dev/null 2>&1
}

tag_and_release() {
    local tag=$1
    if tag_exists_on_remote "$tag" && release_exists "$tag"; then
        echo "Tag $tag and its GitHub release already exist - skipping."
        return 0
    fi
    echo "Creating tag $tag and GitHub release..."
    # Reuse a local tag left by a previous run whose push failed, rather than
    # aborting on "tag already exists"; only create it when absent.
    if ! git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
        git tag -a "$tag" -m "Release $tag"
    fi
    # Push only when the tag is not already on the remote (a prior run may
    # have pushed the tag but failed before creating the release).
    if ! tag_exists_on_remote "$tag"; then
        git push origin "$tag"
    fi
    # Create the release only when missing, so retrying after a failed
    # `gh release create` is idempotent.
    if ! release_exists "$tag"; then
        gh release create "$tag" --generate-notes --title "$tag"
    fi
}

# One "name version" line per publishable workspace member. --no-deps excludes
# dependencies; the `.publish != []` filter drops members with `publish = false`
# (e.g. the wasm example), since those are never released.
members=$(cargo metadata --no-deps --format-version 1 \
    | jq -r '.packages[] | select(.publish != []) | "\(.name) \(.version)"')

while read -r name version; do
    [ -z "$name" ] && continue
    if [ "$name" = "$ROOT_CRATE" ]; then
        tag="v$version"
    else
        tag="$name-v$version"
    fi
    tag_and_release "$tag"
done <<< "$members"
