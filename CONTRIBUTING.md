# Contributing

Thanks for contributing!


## Getting Started

- [Install rust](https://www.rust-lang.org/en-US/install.html)
- `cargo build`


## Making Changes

- After making changes, be sure to run the tests (see below) and run `cargo fmt`!
- Add an entry to the CHANGELOG
- This crate makes use of [`cargo-readme`](https://github.com/livioribeiro/cargo-readme) (`cargo install cargo-readme`)
  to generate the `README.md` from the crate level documentation in `src/lib.rs`.
  This means `README.md` should never be modified by hand.
  Changes should be made to the crate documentation in `src/lib.rs` and the `readme.sh` script run.


## Running Lints/Formatting/Tests

```bash
./lint-test.sh
```


## Submitting Changes

Pull Requests should be made against master.
Travis CI will run the test suite on all PRs.
Remember to update the changelog!

