# Contributing

Thanks for contributing!

## Getting Started

### Required software

- [Rust toolchain](https://www.rust-lang.org/en-US/install.html)
- [`cargo-readme`](https://github.com/livioribeiro/cargo-readme) (`cargo install
  cargo-readme`)
- [GNU Make](https://www.gnu.org/software/make/)
- [Docker](https://www.docker.com/) or another Docker-compatible container
  engine
  - The docker command used by the Makefile can be specified with `DOCKER_COMMAND`, e.g.
    ```
    make DOCKER_COMMAND=containerd docker/redis
    ```

## Making Changes

- Before committing changes, run `make fmt` to format the code
- Add an entry to `CHANGELOG.md` describing what changed and why
- The `README.md` is generated from `src/lib.rs` using `cargo-readme` and must
  never be edited by hand. After changing `src/lib.rs`, run `make docs` to sync
  it, then verify with `make check/readme`
- Keep `make help` output up to date with any Makefile target changes, and
  verify it with `make check/help`
- Run the full CI check before submitting: `make ci`

## Make goals overview

```bash
# Run the full CI pipeline (fmt + clippy + readme check + tests + examples)
make ci
# List all supported Make targets
make help
# Run all tests across all feature combinations
make tests
# Run all examples
make examples
# Sync README.md from src/lib.rs
make docs
# Format the source code
make fmt
# Run all checks (formatting, clippy, README sync)
make check
# Verify `make help` covers every supported target
make check/help
# Remove all generated artifacts and Docker containers
make clean
```

## Submitting Changes

Pull requests should be made against `master`. GitHub Actions will run the full
test suite on all PRs. Remember to update the changelog!
