#!/usr/bin/env bash

set -ex

export RUST_BACKTRACE=1

cargo fmt -- --check
./readme.sh check

cargo clippy --all-features --all-targets --examples --tests
cargo test

# setup redis and env variable and run redis tests
docker rm -f cached-tests || true
docker run --rm --name cached-tests -p 6379:6379 -d redis
REDIS_CS=redis://127.0.0.1/ cargo test --no-default-features --features="redis" -- --test-threads=1 --nocapture
docker rm -f cached-tests || true

for ex in examples/*; do
    base=$(basename $ex)
    exname=$(echo $base | cut -d . -f 1)
    cargo run --example $exname
done

