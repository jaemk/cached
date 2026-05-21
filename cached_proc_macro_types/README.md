Types used by the [`cached`](https://crates.io/crates/cached) proc-macro crate.

Currently exports `Return<T>`, a wrapper that lets callers of `#[cached(with_cached_flag = true)]`
functions inspect whether the returned value came from the cache (`was_cached: bool`).

See the [cached crate](https://crates.io/crates/cached) for full documentation and usage examples.
