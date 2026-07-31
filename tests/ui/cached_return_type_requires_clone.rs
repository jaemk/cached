use cached::macros::cached;

// `#[cached]` clones the cached value on every cache hit (`.to_owned()`) and
// on insert (`.clone()` into the store), so the return type must implement
// `Clone`. The dedicated assertion emits a clear diagnostic spanned at the
// return type ahead of the opaque errors that still follow from deep inside
// macro-generated internals.
struct NotClone {
    _value: u32,
}

#[cached]
fn not_clone_return() -> NotClone {
    NotClone { _value: 0 }
}

fn main() {}
