#!/bin/bash

# Publish order:
# 1. cached_proc_macro_types
# 2. cached_proc_macro
# 3. cached (root)

# Total number of crates
TOTAL_CRATES=3
SUCCESS_COUNT=0

publish_crate() {
    local dir=$1
    echo "Publishing crate in directory: $dir..."
    if (cd "$dir" && cargo publish); then
        echo "Successfully published crate in $dir"
        ((SUCCESS_COUNT++))
    else
        echo "Failed to publish crate in $dir"
    fi
}

publish_crate "cached_proc_macro_types"
publish_crate "cached_proc_macro"
publish_crate "."

if [ $SUCCESS_COUNT -gt 0 ]; then
    echo "At least one crate published successfully ($SUCCESS_COUNT/$TOTAL_CRATES)."
    exit 0
else
    echo "All cargo publish commands failed."
    exit 1
fi
