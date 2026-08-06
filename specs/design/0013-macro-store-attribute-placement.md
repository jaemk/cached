# 0013 - Friendly rejection of store attrs on `#[cached]`

Status: Implemented

## Current state

- `disk` and `redis` store selectors exist only on `#[concurrent_cached]`. The `#[cached]` and
  `#[once]` argument structs use `#[derive(FromMeta)]` with no such fields
  (`cached_proc_macro/src/cached.rs:29`), so `#[cached(disk = true)]` already fails, but with
  darling's generic "Unknown field: `disk`" error.
- `#[concurrent_cached]` already has the reverse: `reject_cached_only_attrs`
  (`cached_proc_macro/src/concurrent_cached.rs:151`) emits friendly messages for
  `sync_writes`/`sync_lock`/`result`/etc., pointing the user the right way.

## Design decisions recorded here

**`reject_concurrent_only_attrs` is the mirror image of `reject_cached_only_attrs`.** It runs
before `FromMeta::from_list` on `#[cached]` and `#[once]`, so the friendly redirect replaces
darling's generic unknown-field message. `ty` + `create` stay valid on `#[cached]` (custom
in-memory store), so the rejection targets only attributes that have no meaning off the
concurrent path.

**The intercepted set covers the store-builder knobs, not just the store selectors.** It
started as `disk`, `redis`, and `map_error`. `shards`, `durable`, `disk_dir`, and
`cache_prefix_block` are equally concurrent-only and were falling through to "Unknown field:
`shards`", so they are intercepted too. Each message names the store the attribute configures
and gives the `#[concurrent_cached(...)]` spelling to switch to.

`reject_cached_only_attrs` on `#[concurrent_cached]` already covers its own full set
(`result`, `option`, `sync_writes`, `sync_writes_buckets`, `sync_lock`, `unsync_reads`), and
`#[once]` rejects the remaining `#[cached]`-only attributes inline, so the three macros now
redirect symmetrically.

## Notes

- No functional change to which attributes are accepted; this is an error-message improvement so
  users land on the correct macro.
- `cached_proc_macro/src/helpers.rs` holds the scan and its unit tests; the rendered messages
  are pinned by the `*_concurrent_only` trybuild cases in `tests/ui/`.
