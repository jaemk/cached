# Image for `make tests/32bit/docker`: the toolchain pinned in rust-toolchain.toml plus the
# i686 target and the 32-bit libc headers that `make tests/32bit` needs. `jq` is for the
# `bin/` scripts that `tests/release_scripts.rs` runs.
FROM rust:1.96.0

RUN apt-get update \
    && apt-get install -y --no-install-recommends gcc-multilib jq \
    && rm -rf /var/lib/apt/lists/*

# Installed as root here because the tests run as the host user, who cannot write to
# RUSTUP_HOME. Keep this in sync with rust-toolchain.toml.
RUN rustup component add rustfmt clippy rust-src \
    && rustup target add wasm32-unknown-unknown i686-unknown-linux-gnu
