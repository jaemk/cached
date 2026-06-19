################################################################################
# Author: Altair Bueno <business.altair.bueno@outlook.com>
# Date: 22/06/2022
# Source: https://github.com/jaemk/cached
# Copyright: MIT License (see LICENSE)
# Description: GNU Makefile for `cached`
################################################################################
# Configuration variables

# List with all basic examples. An example is considered basic if it can be
# run using `cargo run --example=$EXAMPLE` and run standalone. All features are
# **enabled**
CACHED_BASIC_EXAMPLES = async_std \
                        basic \
                        kitchen_sink \
                        tokio \
                        sharded \
                        sharded_expiring \
                        expiring_sized_cache \
                        disk \
                        disk_async
# Same as `CACHED_BASIC_EXAMPLES`, but these examples require the `docker/redis`
# goal
CACHED_REDIS_EXAMPLES = redis \
                        redis-async-tokio \
                        redis-async-async-std \
                        redis-client-side-cache-tokio
# Custom commands. NOTE: You'll need to specify the goal manually. See
# `examples/cargo/wasm` for an example
CACHED_CARGO_EXAMPLES = wasm

CACHED_BASIC_EXAMPLE_TARGETS = $(addprefix examples/basic/, $(CACHED_BASIC_EXAMPLES))
CACHED_REDIS_EXAMPLE_TARGETS = $(addprefix examples/redis/, $(CACHED_REDIS_EXAMPLES))
CACHED_CARGO_EXAMPLE_TARGETS = $(addprefix examples/cargo/, $(CACHED_CARGO_EXAMPLES))

EXAMPLE_TARGETS = examples \
                  examples/basic \
                  examples/cargo \
                  examples/redis \
                  $(CACHED_BASIC_EXAMPLE_TARGETS) \
                  $(CACHED_CARGO_EXAMPLE_TARGETS) \
                  $(CACHED_REDIS_EXAMPLE_TARGETS)

TEST_TARGETS = tests \
               tests/no-default \
               tests/default \
               tests/proc-macro \
               tests/time-stores \
               tests/ahash \
               tests/async \
               tests/disk-store \
               tests/disk-store-sync \
               tests/redis \
               tests/redis-connection-manager \
               tests/redis-async-cache \
               tests/redis-async-cache-tokio \
               tests/redis-async-cache-rustls \
               tests/redis-store \
               tests/redis-store-standalone \
               tests/redis-tokio \
               tests/all-features

DOCKER_TARGETS = docker/status docker/redis
DOC_TARGETS = docs docs/readme
CHECK_TARGETS = check check/fmt check/readme check/clippy check/help
CLEAN_TARGETS = clean clean/docker clean/cargo clean/docker/$(DOCKER_REDIS_CONTAINER_NAME)
HELP_TARGETS = help ci bench $(EXAMPLE_TARGETS) $(TEST_TARGETS) $(DOCKER_TARGETS) $(DOC_TARGETS) fmt $(CHECK_TARGETS) $(CLEAN_TARGETS)

# Cargo command used to run `run`, `build`, `test`... Useful if you keep
# multiple cargo versions installed on your machine
CARGO_COMMAND         = cargo

# Compiler program and flags used to generate README.md
README_CC             = $(CARGO_COMMAND) readme
README_CCFLAGS        = --no-indent-headings

# Compiler program and flags used to generate format the crate
FMT_CC                = $(CARGO_COMMAND) fmt
FMT_CCFLAGS           =

# Docker configuration. Set DOCKER_COMMAND on your shell to override the
# container engine used
#
# ```sh
# # Using containerd to run `docker/redis`
# make DOCKER_COMMAND=containerd docker/redis
# ```
DOCKER_COMMAND                        = docker
DOCKER_REDIS_CONTAINER_NAME           = cached-tests
DOCKER_REDIS_CONTAINER_LOCAL_PORT     = 6399

################################################################################
# Exported variables
export CACHED_REDIS_CONNECTION_STRING = redis://127.0.0.1:$(DOCKER_REDIS_CONTAINER_LOCAL_PORT)
export RUST_BACKTRACE                 = 1

################################################################################
# GitHub Actions goal. Run this to test your changes before submitting your final
# pull request
ci: check tests examples ## Run the full CI pipeline (checks, tests, examples)

bench: ## Run the standardized cache benchmarks
	$(CARGO_COMMAND) bench --bench cache_benches

help: ## List all supported Make targets
	@for target in $(HELP_TARGETS); do \
		case "$$target" in \
			help) desc="List all supported Make targets" ;; \
			ci) desc="Run the full CI pipeline (checks, tests, examples)" ;; \
			bench) desc="Run the standardized cache benchmarks" ;; \
			examples) desc="Run all examples" ;; \
			examples/basic) desc="Run all basic examples" ;; \
			examples/cargo) desc="Build all cargo-project examples" ;; \
			examples/redis) desc="Run all Redis-backed examples" ;; \
			examples/basic/*) desc="Run basic example '$${target#examples/basic/}' with all features" ;; \
			examples/cargo/*) desc="Build cargo example '$${target#examples/cargo/}'" ;; \
			examples/redis/redis-async-async-std) desc="Run async-std Redis example with redis_smol and proc_macro features" ;; \
			examples/redis/*) desc="Run Redis example '$${target#examples/redis/}' with all features" ;; \
			tests) desc="Run the full test matrix" ;; \
			tests/no-default) desc="Run tests with no default features" ;; \
			tests/default) desc="Run tests with the default feature set" ;; \
			tests/proc-macro) desc="Run tests with only the proc_macro feature" ;; \
			tests/time-stores) desc="Run tests with proc_macro and time_stores" ;; \
			tests/async) desc="Run async tests with proc_macro and time_stores" ;; \
			tests/ahash) desc="Run tests with proc_macro and ahash (no time_stores)" ;; \
			tests/disk-store) desc="Run disk_store tests with proc_macro and async runtime" ;; \
			tests/disk-store-sync) desc="Run disk_store tests with proc_macro (no async runtime)" ;; \
			tests/redis) desc="Run all Redis-backed test targets" ;; \
			tests/redis-connection-manager) desc="Check standalone redis_connection_manager feature compilation" ;; \
			tests/redis-async-cache) desc="Check standalone redis_async_cache feature compilation" ;; \
			tests/redis-async-cache-tokio) desc="Check redis_async_cache with redis_tokio_native_tls" ;; \
			tests/redis-async-cache-rustls) desc="Check redis_async_cache with redis_tokio_rustls" ;; \
			tests/redis-store) desc="Run synchronous Redis store tests" ;; \
			tests/redis-store-standalone) desc="Check redis_store feature compilation without proc_macro" ;; \
			tests/redis-tokio) desc="Run async Redis Tokio tests" ;; \
			tests/all-features) desc="Run tests with all features enabled" ;; \
			docker/status) desc="Check whether the Docker engine is available" ;; \
			docker/redis) desc="Start the Redis test container" ;; \
			docs) desc="Sync generated documentation artifacts" ;; \
			docs/readme) desc="Regenerate README.md from src/lib.rs" ;; \
			fmt) desc="Format the source code" ;; \
			check) desc="Run all verification checks" ;; \
			check/fmt) desc="Verify formatting without changing files" ;; \
			check/readme) desc="Verify README.md matches src/lib.rs" ;; \
			check/clippy) desc="Run clippy across all targets, examples, and tests" ;; \
			check/help) desc="Verify the help output covers every supported target" ;; \
			clean) desc="Remove all generated artifacts and Docker containers" ;; \
			clean/cargo) desc="Run cargo clean" ;; \
			clean/docker) desc="Remove managed Docker containers" ;; \
			clean/docker/*) desc="Remove Docker container '$${target#clean/docker/}'" ;; \
			*) desc="" ;; \
		esac; \
		if [ -z "$$desc" ]; then \
			echo "Missing help text for $$target" >&2; \
			exit 1; \
		fi; \
		printf "%-30s %s\n" "$$target" "$$desc"; \
	done

################################################################################
.check-examples-expanded:
	@output="$$( $(MAKE) -n --no-print-directory examples/basic examples/cargo examples/redis )"; \
	echo "$$output" | grep -Eq 'run --example|build --target' || (>&2 echo 'Example targets did not expand to runnable commands'; exit 1)

# Runs all examples
examples: .check-examples-expanded examples/basic examples/cargo examples/redis
# Runs all basic examples
examples/basic: $(CACHED_BASIC_EXAMPLE_TARGETS)
# Runs all the project based examples
examples/cargo: $(CACHED_CARGO_EXAMPLE_TARGETS)
# Runs `redis` related examples. NOTE: depends on `docker/redis`
examples/redis: $(CACHED_REDIS_EXAMPLE_TARGETS)

examples/basic/%:
	@echo [$@]: Running example $*...
	$(CARGO_COMMAND) run --example $* --all-features

# Only builds the `wasm` example. Running this example requires a browser
examples/cargo/wasm:
	@echo [$@]: Building example $*...
	cd examples/wasm ; $(CARGO_COMMAND) build --target=wasm32-unknown-unknown

# async-std + smol redis example: run with explicit features only to avoid
# mixing tokio and smol runtimes that --all-features would enable together.
examples/redis/redis-async-async-std: docker/redis
	@echo [$@]: Running example redis-async-async-std...
	$(CARGO_COMMAND) run --example redis-async-async-std --features "redis_smol_native_tls,proc_macro"

examples/redis/%: docker/redis
	@echo [$@]: Running example $*...
	$(CARGO_COMMAND) run --example $* --all-features

################################################################################
# Runs `cached` tests with various feature combinations.
# Non-Redis targets run first; Redis targets (which need Docker) run last.
tests: tests/no-default tests/default tests/proc-macro tests/time-stores tests/ahash tests/async tests/disk-store tests/disk-store-sync tests/redis

# No features at all — only store tests compile
# --tests skips doc-tests that require proc_macro/other features to compile
tests/no-default:
	@echo "[$@]: Running tests (no default features)..."
	$(CARGO_COMMAND) test --no-default-features --tests -- --nocapture

# Default feature set: proc_macro + ahash + time_stores
tests/default:
	@echo "[$@]: Running tests (default features)..."
	$(CARGO_COMMAND) test -- --nocapture

# proc_macro only (no time_stores, no ahash)
tests/proc-macro:
	@echo "[$@]: Running tests (proc_macro only)..."
	$(CARGO_COMMAND) test --no-default-features --features proc_macro --tests -- --nocapture

# time_stores + proc_macro (no ahash, no async)
tests/time-stores:
	@echo "[$@]: Running tests (time_stores + proc_macro)..."
	$(CARGO_COMMAND) test --no-default-features --features "proc_macro,time_stores" --tests -- --nocapture

# async + proc_macro + time_stores (tokio in dev-deps supplies the test runtime)
tests/async:
	@echo "[$@]: Running tests (async + proc_macro + time_stores)..."
	$(CARGO_COMMAND) test --no-default-features --features "proc_macro,time_stores,async" --tests -- --nocapture

# proc_macro + ahash (no time_stores)
tests/ahash:
	@echo "[$@]: Running tests (proc_macro + ahash, no time_stores)..."
	$(CARGO_COMMAND) test --no-default-features --features "proc_macro,ahash" --tests -- --nocapture

# disk_store + proc_macro (+ async for async disk tests; tokio dev-dep supplies the test runtime)
tests/disk-store:
	@echo "[$@]: Running tests (disk_store + proc_macro + async)..."
	$(CARGO_COMMAND) test --no-default-features --features "proc_macro,disk_store,async" --tests -- --nocapture

# disk_store + proc_macro (no async runtime)
tests/disk-store-sync:
	@echo "[$@]: Running tests (disk_store + proc_macro, no async)..."
	$(CARGO_COMMAND) test --no-default-features --features "proc_macro,disk_store" --tests -- --nocapture

# Redis targets. The runtime targets (redis-store, redis-tokio, all-features)
# each take an order-only `| docker/redis` prerequisite so the container is
# guaranteed up before they run *regardless of `make -j`* — a plain prerequisite
# ordering on the aggregate below is not honored under parallel make. The
# standalone `*-async-cache*`/`connection-manager` targets are compile-only
# `cargo check`s and need no container.
tests/redis: tests/redis-connection-manager tests/redis-async-cache tests/redis-async-cache-tokio tests/redis-async-cache-rustls tests/redis-store-standalone tests/redis-store tests/redis-tokio tests/all-features

tests/redis-store-standalone:
	@echo "[$@]: Checking standalone redis_store feature compilation..."
	$(CARGO_COMMAND) check --no-default-features --features redis_store

tests/redis-connection-manager:
	@echo "[$@]: Checking standalone redis_connection_manager feature compilation..."
	$(CARGO_COMMAND) check --no-default-features --features redis_connection_manager

tests/redis-async-cache:
	@echo "[$@]: Checking standalone redis_async_cache feature compilation..."
	$(CARGO_COMMAND) check --no-default-features --features redis_async_cache

tests/redis-async-cache-tokio:
	@echo "[$@]: Checking redis_async_cache with redis_tokio_native_tls..."
	$(CARGO_COMMAND) check --no-default-features --features "redis_tokio_native_tls,redis_async_cache"

tests/redis-async-cache-rustls:
	@echo "[$@]: Checking redis_async_cache with redis_tokio_rustls..."
	$(CARGO_COMMAND) check --no-default-features --features "redis_tokio_rustls,redis_async_cache"

# Synchronous Redis store only
tests/redis-store: | docker/redis
	@echo "[$@]: Running tests (redis_store + proc_macro)..."
	$(CARGO_COMMAND) test --no-default-features --features "proc_macro,redis_store" --tests -- --nocapture

# Async Redis via Tokio with native-tls (tokio dev-dep supplies the test runtime)
tests/redis-tokio: | docker/redis
	@echo "[$@]: Running tests (redis_tokio_native_tls + proc_macro + time_stores)..."
	$(CARGO_COMMAND) test --no-default-features --features "proc_macro,time_stores,redis_tokio_native_tls" --tests -- --nocapture

# Full all-features run
tests/all-features: | docker/redis
	@echo "[$@]: Running tests (all features)..."
	$(CARGO_COMMAND) test --all-features -- --nocapture

################################################################################
# Starts a Redis server using `DOCKER_COMMAND`
docker/redis: docker/status
	@echo [$@]: Starting Redis container...
	-$(DOCKER_COMMAND) run --rm --name $(DOCKER_REDIS_CONTAINER_NAME) \
 		-p $(DOCKER_REDIS_CONTAINER_LOCAL_PORT):6379 -d redis

docker/status:
	@echo [$@]: Checking the Docker engine
	@docker info > /dev/null || (>&2 echo 'Is the Docker engine running?' && exit 42)

################################################################################
# Syncs all docs
docs: docs/readme

# Updates README.md using `README_CC`
docs/readme: README.md

README.md: src/lib.rs
	@echo [$@]: Updating $@...
	$(README_CC) $(README_CCFLAGS) > $@

################################################################################
# Formats `cached` crate
fmt:
	@echo [$@]: Formatting code...
	$(FMT_CC) $(FMT_CCFLAGS)

################################################################################
# Runs all checks
check: check/fmt check/readme check/clippy check/help

# Checks if `cached` crate is well formatted
check/fmt: FMT_CCFLAGS += --check
check/fmt:
	@echo [$@]: Checking code format...
	$(FMT_CC) $(FMT_CCFLAGS)

# Checks if the README.md file is up-to-date
check/readme:
	@echo [$@]: Checking README.md...
	$(README_CC) $(README_CCFLAGS) > _tmp_readme.md
	cmp README.md _tmp_readme.md
	rm -f _tmp_readme.md

# Runs clippy linter on `cached` crate
check/clippy:
	@echo [$@]: Running clippy...
	$(CARGO_COMMAND) clippy --all-features --all-targets --examples --tests -- -D warnings

# Verifies that `make help` documents every supported target
check/help:
	@echo [$@]: Checking help coverage...
	@expected="$$(printf '%s\n' $(HELP_TARGETS) | sort -u)"; \
	documented="$$( $(MAKE) --no-print-directory help | awk '{print $$1}' | sort -u )"; \
	if [ "$$expected" != "$$documented" ]; then \
		echo "Expected targets:" >&2; \
		printf '%s\n' "$$expected" >&2; \
		echo "Documented targets:" >&2; \
		printf '%s\n' "$$documented" >&2; \
		exit 1; \
	fi

################################################################################
# Cleans all generated artifacts and deletes all docker containers
clean: clean/docker clean/cargo

# Runs `cargo clean`
clean/cargo:
	@echo [$@]: Removing cargo artifacts...
	$(CARGO_COMMAND) clean

# Removes all docker containers
clean/docker: clean/docker/$(DOCKER_REDIS_CONTAINER_NAME)

# Removes a docker container with the given name
clean/docker/%:
	@echo [$@]: Removing container called $*...
	$(DOCKER_COMMAND) rm -f $*

################################################################################
# Special targets.

# Derived from HELP_TARGETS so generated per-example / per-test targets
# (CACHED_*_EXAMPLE_TARGETS, the redis-async-cache* / redis-connection-manager
# test targets) stay declared phony automatically and cannot drift.
.PHONY: $(HELP_TARGETS) .check-examples-expanded
