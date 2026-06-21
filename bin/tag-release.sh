#!/bin/bash

# Create a git tag and GitHub release for every workspace crate whose current
# version is not yet tagged on the remote.
#
# Idempotent: a crate whose tag/release already exists is skipped. The script
# tags every publishable workspace crate that lacks a tag or release, which
# includes backfilling crates published in earlier runs as well as those just
# published. It leaves crates that are already fully tagged and released alone.
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
    # `git ls-remote` output format is "<sha>\trefs/tags/<name>".  Match the
    # tab-prefixed ref name as a fixed string so dots in tag names (e.g.
    # "v1.0.0") are not treated as regex metacharacters, and the leading tab
    # ensures we match only the full ref field with no substring false positives
    # (e.g. "v1.0.0" would not match "v1.0.0-rc1" because that tag would appear
    # as "	refs/tags/v1.0.0-rc1", not containing "	refs/tags/v1.0.0" verbatim).
    git ls-remote --tags origin "refs/tags/$1" | grep -qF -- "	refs/tags/$1"
}

release_exists() {
    gh release view "$1" >/dev/null 2>&1
}

tag_and_release() {
    local tag=$1
    # Cache both remote checks up front so each network call runs at most once.
    local tag_remote release_remote
    tag_exists_on_remote "$tag" && tag_remote=true || tag_remote=false
    release_exists "$tag"       && release_remote=true || release_remote=false

    if [ "$tag_remote" = true ] && [ "$release_remote" = true ]; then
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
    if [ "$tag_remote" = false ]; then
        git push origin "$tag"
    fi
    # Create the release only when missing, so retrying after a failed
    # `gh release create` is idempotent.
    if [ "$release_remote" = false ]; then
        gh release create "$tag" --generate-notes --title "$tag"
    fi
}

# One "name version" line per publishable workspace member. --no-deps excludes
# dependencies; the `.publish != []` filter drops members with `publish = false`
# (e.g. the wasm example), since those are never released.
#
# cargo-metadata contract: `publish = false` serializes as `"publish": []`,
# while an absent publish field (meaning "publish to all registries") serializes
# as `"publish": null`.  Comparing `!= []` therefore keeps null (publishable)
# and drops [] (explicitly suppressed) without treating them the same way.
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
