# 0021 - Amortize redb refresh-on-hit write txns

Status: Needs research

## Current state

- redb `cache_get` with refresh_on_hit opens a write transaction on every hit to rewrite
  `created_at` (`src/stores/redb.rs:560`), turning reads into serialized writes.
- Each async op runs one redb transaction on the global blocking pool
  (`src/stores/redb.rs:852`).

## Desired work

- Store an absolute expiry timestamp and only rewrite when the remaining TTL crosses a
  threshold, amortizing the refresh.
- Bump DISK_FILE_VERSION if the on-disk representation changes (old files recomputed).

## Notes

- Amortized refresh weakens the "every hit resets the clock" guarantee slightly; document the
  contract.
- The blocking-pool-saturation concern is doc-only for now.
