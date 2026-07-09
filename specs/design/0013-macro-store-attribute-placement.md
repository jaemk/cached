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

## Desired work

- Add the mirror-image check on `#[cached]` (and `#[once]`): detect the concurrent-store-only
  attributes (`disk`, `redis`, and any others that only make sense on the concurrent path) and
  emit a clear compile error directing the user to `#[concurrent_cached]`, instead of darling's
  generic unknown-field message.
- Confirm `ty` + `create` remain valid on `#[cached]` (custom in-memory store), so the rejection
  targets only the I/O-backed store selectors.

## Notes

- No functional change to which attributes are accepted; this is an error-message improvement so
  users land on the correct macro.
