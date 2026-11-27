//! Run with `cargo run --example io_compression --features redb_store,compression_zstd`.
//! RedisCacheBuilder and AsyncRedisCacheBuilder expose the same compression settings.

use cached::prelude::*;
use cached::{Compression, RedbCache};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let report = "monthly report\n".repeat(1024);
    let small = "summary".to_string();

    {
        let cache = RedbCache::<u64, String>::builder("reports")
            .disk_dir(dir.path())
            .compression(Compression::Zstd { level: 3 })
            .min_compress_size(256)
            .max_decompressed_size(1024 * 1024)
            .build()?;

        cache.cache_set_ref(&1, &report)?;
        cache.cache_set_ref(&2, &small)?;
    }

    // The Cargo feature lets this reader open both formats. New writes are raw
    // because this builder has no compression algorithm configured.
    let cache = RedbCache::<u64, String>::builder("reports")
        .disk_dir(dir.path())
        .build()?;
    assert_eq!(cache.cache_get(&1)?, Some(report));
    assert_eq!(cache.cache_get(&2)?, Some(small));
    Ok(())
}
