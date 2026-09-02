// Negative surface for a deliberate NON-implementation. `CacheSetMaxSize` is carried only by the
// four bounded single-owner stores (`LruCache`, `LruTtlCache`, `ExpiringLruCache`,
// `TtlSortedCache`); `UnboundCache`, `TtlCache` and `ExpiringCache` are left off it on purpose,
// because none of them has a live bound to change. The crate docs make that an explicit design
// decision -- "A stub returning `None` would be indistinguishable to a generic caller from a store
// that really resized, so the capability lives in its own trait and those stores are simply left
// off it" -- so a generic `T: CacheSetMaxSize` helper must REJECT an `ExpiringCache` at compile
// time rather than accept it and answer `None` at runtime.
//
// Same shape as `sharded_unbound_no_set_ttl.rs`, one level up: there the store is missing from the
// trait so the method does not resolve; here the bound is what fails. If a future change added a
// stub impl for `ExpiringCache`, this would start compiling and the golden would break, flagging
// the regression.
use cached::{CacheSetMaxSize, Expires, ExpiringCache};

#[derive(Clone)]
struct Token;

impl Expires for Token {
    fn is_expired(&self) -> bool {
        false
    }
}

/// The generic caller the design decision is about: it can only be handed a store that really
/// resizes.
fn shrink<T: CacheSetMaxSize>(cache: &mut T, max_size: usize) -> Option<usize> {
    cache.set_max_size(max_size)
}

fn main() {
    let mut cache: ExpiringCache<String, Token> = ExpiringCache::new();
    let _ = shrink(&mut cache, 10);
}
