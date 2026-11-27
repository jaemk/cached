# 0057 - Opt-in value compression for the IO stores

Status: Implemented

## Background

- Issue #222 asks for opt-in compression of stored values so more entries fit under a redis
  `maxmemory` limit (gzip or lz4 suggested). A later comment asks for it on the other stores too.
- `RedisCache`/`AsyncRedisCache` and `RedbCache` originally stored the MessagePack envelope from REDIS-7 /
  REDB-6 as raw bytes. The issue predates 3.0: entries are no longer JSON, which removes some of
  the size gap compression would close, but repetitive payloads still compress well.
- The raw decoder uses `rmp_serde::from_slice`. Redis also has a fallback to the pre-3.0 JSON
  envelope (`deserialize_cached_redis_value`, 0031). Undecodable entries self-heal by default
  (0029).

## Scope

- **IO stores only.** The in-memory stores keep `V` as-is and hand out `&V`, so compressing there
  would need `V: Serialize` plus a decode on every hit, which is a different store. A caller who
  wants compressed in-memory values can store `Vec<u8>` (or a newtype) and compress in their own
  code.
- **Independent of 0011.** Compression wraps the bytes the codec produces, so it does not need
  the pluggable-codec work and does not change the envelope.

## Design

- **Builder option, behind a cargo feature.** For example `.compression(Compression::Zstd { level })`
  on `RedisCacheBuilder`, `AsyncRedisCacheBuilder`, and `RedbCacheBuilder`, gated by a
  `compression_zstd` feature. Omit the builder setting to write uncompressed entries.
- **Self-identifying frames, no envelope bump.** The zstd frame magic is `28 B5 2F FD`;
  raw MessagePack starts with `0x92` and legacy Redis JSON starts with `{`. When the feature
  is compiled in, readers accept frames even when configured to write raw entries.
- **Write threshold and size limit.** `min_compress_size` defaults to 256 serialized bytes.
  Compress at or above the threshold, up through `max_decompressed_size` (default 64 MiB),
  and keep the result only when it is smaller. Larger envelopes are written raw, so a
  store can read everything it writes using its own limit.
- **Bounded decompression.** Check the frame's declared content size before allocation.
  Decode into a buffer bounded by that size and reject frames with missing sizes or sizes
  above the configured cap. Reads report a frame above the reader's cap or an allocation
  failure as a deserialization error in both modes and preserve the entry. redb expiry sweeps
  and statistics scans also report these errors. The cap applies to compressed entries only.
- **Errors.** Compression failure on write is a `CacheSerialization` error, and decompression
  failure on read is a `CacheDeserialization` error, both with a boxed source so the codec
  crate does not enter the public API (the redb 5 pattern).

## Algorithm

One algorithm first, with an enum left open for more:

- **zstd** (`zstd` crate): best ratio at comparable speed, and the level is tunable. It binds the C
  library (`zstd-sys`), so it needs a C toolchain. That is acceptable for redis/redb, which do not
  target wasm.
- **lz4** (`lz4_flex`): pure Rust and faster, but with a lower ratio. Use its frame format (`frame`
  module), not the block format, which has no magic bytes.

The implementation uses zstd to prioritize entries per byte of Redis memory. The C dependency
is enabled by `compression_zstd`.

## Caveats

- **Mixed-version fleets.** Upgrade every Redis reader with `compression_zstd` before
  enabling compressed writes. An older build or a build without the feature treats frames as
  undecodable. Default self-healing deletes those entries, and strict mode reports an error.
  Redb uses an exclusive file lock, so this matters most for Redis deployments.
- **Synchronous Redis pooling.** Serialize and compress new values before checking out a
  connection. Return the connection after fetching or removing the value and before decoding it.
  A read that needs to self-heal acquires a connection for the conditional delete.
- **CPU on async executors.** Async Redis runs compression on the caller's executor.
  Async redb runs compression on its blocking pool.
- redb refresh and lookup-triggered expiry reuse the decoded value when the stored bytes
  match the read snapshot. `RedbCache::scan_stats` (0056) decompresses each framed entry
  while scanning its table.

## Notes

- Additive: a feature, builder options, and a read-path branch. No breaking change.
- Coverage includes standard frame magic, mixed formats, threshold and cap boundaries,
  malformed frames, Redis legacy JSON, and redb refresh, reopen, and scans.
- CI runs `make tests/io-compression` across builds with and without zstd support. Sync and
  async writers share a Redis namespace and a redb file across the builds. Readers verify raw
  and framed values. The zstd build checks that a reader with a lower limit preserves entries
  it cannot decode. The build without zstd checks strict errors and self-healing.
- Related: 0011 (codec), 0029 (self-healing), 0031 (redis read gate), REDIS-7, REDB-6.
