use cached::macros::cached;

// `#[cached]` clones the cached value on insert into the store and on every
// cache hit, so the return type must implement `Clone`. Each of those clones is
// a `<Ret as Clone>::clone` call spanned at the return type, which is the
// `Clone` assertion and the clone at once: the golden pins ONE precisely-spanned
// error. It used to be three - the assertion plus an E0308/E0599 cascade from
// the `.clone()` / `.to_owned()` method calls the body used to emit, both
// spanned at the `#[cached]` attribute (0043a).
struct NotClone {
    _value: u32,
}

#[cached]
fn not_clone_return() -> NotClone {
    NotClone { _value: 0 }
}

fn main() {}
