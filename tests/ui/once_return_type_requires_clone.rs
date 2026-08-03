use cached::macros::once;

// `#[once]` clones the cached value on every cache-set and on a cache hit
// (`.clone()`), the same failure mode as `#[cached]`, so the return type must
// implement `Clone`. The dedicated assertion emits a clear diagnostic spanned
// at the return type ahead of the opaque errors that still follow from deep
// inside macro-generated internals.
struct NotClone {
    _value: u32,
}

#[once]
fn not_clone_once_return() -> NotClone {
    NotClone { _value: 0 }
}

fn main() {}
