# 0057 - Opt-in value compression for the IO stores

Status: Not implemented

## Current state

- Issue #222 asks for opt-in compression of stored values so more entries fit under a redis
  `maxmemory` limit (gzip or lz4 suggested). A later comment asks for it on the other stores too.
- `RedisCache`/`AsyncRedisCache` and `RedbCache` store the MessagePack envelope from REDIS-7 /
  REDB-6 as raw bytes. The issue predates 3.0: entries are no longer JSON, which removes some of
  the size gap compression would close, but repetitive payloads still compress well.
- Decode is `rmp_serde::from_slice` on the stored bytes. Redis adds a fallback to the pre-3.0 JSON
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
  `compression_zstd` feature (or `compression_lz4`). Off by default. This is a builder setting,
  not a type parameter, the same rule 0011 sets for a codec.
- **Self-identifying frames, no envelope bump.** Write the standard frame format, whose magic
  bytes identify it: zstd `28 B5 2F FD`, lz4 frame `04 22 4D 18`. A stored uncompressed entry always
  starts with `0x92` (a MessagePack 2-element fixarray), and a legacy redis entry starts with `{`.
  On read, a leading frame magic means decompress and then decode; anything else takes today's
  path. So mixed stores keep working: entries written before the option was enabled, entries
  below the size threshold, and entries written after it is turned off again. REDIS-7, REDB-6,
  `REDIS_VALUE_VERSION`, and `DISK_FILE_VERSION` stay unchanged.
- **Size threshold.** `min_compress_size` (default on the order of 256 bytes): smaller envelopes
  are stored uncompressed, since a frame header plus entropy coding is a net loss on short
  values. Because frames identify themselves, the threshold needs no stored flag.
- **Bounded decompression.** Decompress into a buffer capped at a builder-set
  `max_decompressed_size`, so a corrupt or hostile entry cannot allocate unbounded memory. zstd
  exposes this as `zstd::bulk::decompress(data, capacity)`. Exceeding the cap is a
  `CacheDeserialization` error, so it follows the normal self-heal / strict rules.
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

Recommendation: zstd, since the issue's goal is entries per byte of redis memory, and the C
dependency only affects users who enable the feature.

## Caveats

- **Mixed-version fleets.** An older `cached` sharing the redis database cannot decode a
  compressed entry. With the default self-healing it deletes the entry and recomputes, causing
  churn but no wrong values. With `strict_deserialization(true)` it returns an error. Enable
  compression only after every reader is upgraded. A redb file belongs to one process, so this
  is mostly a redis concern.
- **CPU on async executors.** Compressing a large value inside `async_cache_set` runs on the
  executor thread. Document it; offloading is the caller's choice.
- `RedbCache::scan_stats` (0056) decodes every entry, so with compression on it also decompresses
  every entry.

## Notes

- Additive: a feature, builder options, and a read-path branch. No breaking change.
- Tests to pin: a frame-magic golden next to `tests/frozen_format_golden.rs`, mixed
  compressed/uncompressed reads, the threshold, the decompression cap, and an uncompressed
  entry written by 4.1 read back with the option on.
- Related: 0011 (codec), 0029 (self-healing), 0031 (redis read gate), REDIS-7, REDB-6.
