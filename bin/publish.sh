#!/bin/bash

# Publish every workspace crate to crates.io, in dependency order:
# 1. cached_proc_macro_types
# 2. cached_proc_macro
# 3. cached (root)
#
# Exit status is the point of this script. A crate whose current version is
# already on the index is a benign skip: the version did not change since the
# last release, so there is nothing to publish and a re-run of this script must
# stay idempotent. Any OTHER failure is fatal and fails the release.
#
# This distinction matters. An earlier version treated "at least one crate
# published" as success, which reported a green release while two of three
# crates had failed, and the tagging step then ran against a half-published
# workspace. Publishing the root crate while a bumped subcrate silently failed
# would leave `cached` on the index depending on a version of
# `cached_proc_macro*` that does not exist.

set -uo pipefail

ALREADY_PUBLISHED_RE="already exists on crates.io index"

published=0
skipped=0
failed=0

publish_crate() {
    local dir=$1
    local out status
    echo "Publishing crate in directory: $dir..."
    out=$( (cd "$dir" && cargo publish) 2>&1 )
    status=$?

    if [ $status -eq 0 ]; then
        echo "$out"
        echo "Published crate in $dir"
        published=$((published + 1))
        return 0
    fi

    if grep -qF -- "$ALREADY_PUBLISHED_RE" <<<"$out"; then
        echo "Version in $dir is already on the index - nothing to publish."
        skipped=$((skipped + 1))
        return 0
    fi

    echo "$out" >&2
    echo "Failed to publish crate in $dir" >&2
    failed=$((failed + 1))
    return 1
}

# Do not abort on the first failure: attempting the remaining crates makes the
# log show every problem in one run instead of one per re-run.
publish_crate "cached_proc_macro_types" || true
publish_crate "cached_proc_macro" || true
publish_crate "." || true

echo "Publish summary: $published published, $skipped already current, $failed failed."

if [ $failed -gt 0 ]; then
    echo "Release failed: $failed crate(s) could not be published." >&2
    exit 1
fi

exit 0
