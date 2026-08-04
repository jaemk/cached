use cached::macros::concurrent_cached;

// `disk = true` selects the redb-backed store, which lives behind the
// `redb_store` feature of `cached`. The macro cannot see the downstream crate's
// feature flags, so it always emits `cached::__require_redb_store_feature!{}` and
// lets that declarative guard decide: a no-op with `redb_store` on, a
// `compile_error!` naming `redb_store` with it off (0042).
//
// Without the guard the only diagnostic was a raw "cannot find `RedbCache` in
// `cached`" pair (E0433/E0425) naming an internal type and no `cached` feature.
//
// This file is only registered with trybuild when `redb_store` is OFF; with the
// feature on the guard expands to nothing and the file compiles.
#[concurrent_cached(disk = true, map_error = r#"|e| format!("{:?}", e)"#)]
fn disk_without_redb_store(k: i32) -> Result<i32, String> {
    Ok(k)
}

fn main() {}
