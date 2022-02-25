#!/usr/bin/env bash

set -ex

export RUST_BACKTRACE=1

docker_cmd="${DOCKER_COMMAND:-docker}"
docker_container_name="${DOCKER_CONTAINER_NAME:-cached-tests}"
docker_redis_local_port="${DOCKER_REDIS_LOCAL_PORT:-6399}"

cargo fmt -- --check
./readme.sh check

cargo clippy --all-features --all-targets --examples --tests

# setup redis and env variable and run redis tests
$docker_cmd rm -f $docker_container_name || true
$docker_cmd run --rm --name $docker_container_name -p $docker_redis_local_port:6379 -d redis
export CACHED_REDIS_CONNECTION_STRING=redis://127.0.0.1:$docker_redis_local_port
cargo test --all-features -- --nocapture

if [[ "$SKIP_EXAMPLES" = "true" ]]; then
    echo "skipping examples"
else
    for ex in examples/*; do
        base=$(basename $ex)
        exname=$(echo $base | cut -d . -f 1)
        if [[ -z "$RUN_EXAMPLE_NAME" ]] || [[ "$RUN_EXAMPLE_NAME" = "$exname" ]]; then
            cargo run --example $exname --all-features
        fi
    done
fi

# clean up
$docker_cmd rm -f $docker_container_name || true
