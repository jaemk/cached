use cached::macros::once;

// `#[once]` clones the cached value on every cache-set and on a cache hit, the
// same failure mode as `#[cached]`, so the return type must implement `Clone`.
// Each of those clones is a `<Ret as Clone>::clone` call spanned at the return
// type, which is the `Clone` assertion and the clone at once: the golden pins
// ONE precisely-spanned error, down from three (the assertion plus an
// E0308/E0599 cascade spanned at the `#[once]` attribute) (0043a).
struct NotClone {
    _value: u32,
}

#[once]
fn not_clone_once_return() -> NotClone {
    NotClone { _value: 0 }
}

fn main() {}
