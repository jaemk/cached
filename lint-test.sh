#!/bin/bash

set -ex

export RUST_BACKTRACE=1

cargo fmt -- --check
./readme.sh check

cargo clippy --all-features --all-targets --examples --tests
cargo test --all-features

