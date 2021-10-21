#!/usr/bin/env bash

set -ex

export RUST_BACKTRACE=1

cargo fmt -- --check
./readme.sh check

cargo clippy --all-features --all-targets --examples --tests
cargo test --all-features

for ex in examples/*; do
    base=$(basename $ex)
    exname=$(echo $base | cut -d . -f 1)
    cargo run --example $exname
done

