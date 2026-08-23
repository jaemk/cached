#!/bin/bash

# Assert the workspace crate versions agree, before anything is published.
#
# The release workflow decides whether to publish from one signal: the root
# crate's version is not on the crates.io index yet. That signal says nothing
# about the subcrates, so two half-bumped states sail straight past it:
#
#   1. The root version is bumped but its `cached_proc_macro*` dependency pins
#      are not. `cargo publish` resolves the stale pins against the index, where
#      the previous version really does exist, so nothing fails: a STABLE
#      `cached` goes out depending on pre-release subcrates. `publish.sh`
#      classifies both subcrates as benign "already on the index" skips, so the
#      release reports green.
#   2. The pins are bumped but the subcrate package versions are not. The
#      subcrate publishes skip as already-published, and the root publish then
#      requires a version that does not exist on the index.
#
# Both are invisible to every other check in the pipeline, and only the first is
# recoverable after the fact (by yanking). Fail the release instead.
#
# Reads `cargo metadata` from the current directory. A metadata JSON file may be
# passed as $1 instead, which is how the tests drive the failure cases.

set -euo pipefail

ROOT_CRATE="cached"
SUBCRATES=("cached_proc_macro" "cached_proc_macro_types")

if [ $# -gt 0 ]; then
    metadata=$(cat "$1")
else
    metadata=$(cargo metadata --no-deps --format-version 1)
fi

field() {
    jq -r "$@" <<<"$metadata"
}

root_version=$(field --arg root "$ROOT_CRATE" \
    '.packages[] | select(.name == $root) | .version')
if [ -z "$root_version" ] || [ "$root_version" = "null" ]; then
    echo "Could not determine the $ROOT_CRATE version from cargo metadata." >&2
    exit 1
fi

status=0
for sub in "${SUBCRATES[@]}"; do
    package_version=$(field --arg sub "$sub" \
        '.packages[] | select(.name == $sub) | .version')
    # `version = "3.0.0"` in Cargo.toml reaches metadata as the requirement
    # "^3.0.0"; the pins are exact single versions, so stripping the caret is
    # enough to compare them against a package version.
    requirement=$(field --arg root "$ROOT_CRATE" --arg sub "$sub" \
        '.packages[] | select(.name == $root) | .dependencies[] | select(.name == $sub) | .req')

    if [ -z "$package_version" ] || [ "$package_version" = "null" ]; then
        echo "Could not determine the $sub version from cargo metadata." >&2
        status=1
        continue
    fi
    if [ -z "$requirement" ] || [ "$requirement" = "null" ]; then
        echo "$ROOT_CRATE does not declare a dependency on $sub." >&2
        status=1
        continue
    fi

    pin=${requirement#^}
    if [ "$pin" != "$package_version" ]; then
        echo "Version mismatch: $ROOT_CRATE depends on $sub $requirement, but the workspace builds $sub $package_version." >&2
        echo "  Bump the [dependencies.$sub] version in Cargo.toml and $sub/Cargo.toml together." >&2
        status=1
        continue
    fi

    # A pre-release root may depend on pre-release subcrates. A stable one may not:
    # publishing that pins a stable release to a version the resolver treats as
    # pre-release for every downstream user.
    if [[ "$root_version" != *-* && "$package_version" == *-* ]]; then
        echo "Pre-release dependency: $ROOT_CRATE $root_version is a stable release but depends on $sub $package_version." >&2
        echo "  Bump $sub to a stable version before releasing $ROOT_CRATE $root_version." >&2
        status=1
    fi
done

if [ $status -ne 0 ]; then
    echo "Workspace versions do not agree - refusing to publish." >&2
    exit 1
fi

echo "Workspace versions agree ($ROOT_CRATE $root_version)."
exit 0
