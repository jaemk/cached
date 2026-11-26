use cached::macros::once;

// `#[once]` has no per-call key, so there is nothing to skip.
#[once]
fn f(id: u32, #[cached_skip] tag: String) -> u32 {
    let _ = tag;
    id
}

fn main() {}
