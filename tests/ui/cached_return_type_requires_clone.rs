use cached::macros::cached;

// `#[cached]` clones the cached value on every cache hit (`.to_owned()`) and
// on insert (`.clone()` into the store), so the return type must implement
// `Clone`. Without the dedicated assertion this only fails deep inside
// macro-generated internals; it should instead produce a clear diagnostic
// spanned at the function's return type.
struct NotClone {
    _value: u32,
}

#[cached]
fn not_clone_return() -> NotClone {
    NotClone { _value: 0 }
}

fn main() {}
