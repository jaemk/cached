# 0025 - redb resolved-path introspection and temp fallback

Status: Needs research

## Current state

- redb's `name` is validated only at build() and the file is `<name>_v<DISK_FILE_VERSION>.redb`
  under a default dir derived from the exe name (`src/stores/redb.rs:193,275`).
- The default-dir logic silently falls back from the system cache dir to the temp dir on
  PermissionDenied (`src/stores/redb.rs:214`), so a cache can land in /tmp without the caller
  knowing.

## Desired work

- Expose the resolved disk path before build (a builder `resolved_disk_path()` mirroring redis's
  resolve_connection_string).
- Make the temp-dir fallback explicit (an opt-in builder flag) or return an error instead of
  silently using a volatile location.

## Notes

- A durable store silently relocating to /tmp is a correctness surprise. Middle ground: an
  explicit `allow_temp_fallback(bool)`.
- Migration: low; most users pass disk_directory explicitly.
- 5.4 refresh: the fallback logic already matches on both `PermissionDenied` and
  `ReadOnlyFilesystem` (`src/stores/redb.rs:272-275`), so a read-only mount also triggers
  the fallback rather than returning a hard error. The explicitness concern applies equally
  to both error kinds.
