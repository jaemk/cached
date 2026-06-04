#!/usr/bin/env bash
# Detect which crates need version bumps based on files changed vs base ref.
#
# Usage: detect-crates.sh [BASE_REF]   (default: origin/master)
#
# Output: one line per crate to bump — "cached", "cached_proc_macro", "cached_proc_macro_types"
# `cached` is always included.

set -euo pipefail

BASE=${1:-origin/master}

changed=$(git diff "$BASE" --name-only)

echo "cached"

if echo "$changed" | grep -q "^cached_proc_macro/"; then
    echo "cached_proc_macro"
fi

if echo "$changed" | grep -q "^cached_proc_macro_types/"; then
    echo "cached_proc_macro_types"
fi
