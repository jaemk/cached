use cached::macros::cached;

// `ty` + `convert` is also a custom key; `#[cached_skip]` is rejected there too.
#[cached(
    ty = "cached::UnboundCache<u32, u32>",
    create = "{ cached::UnboundCache::new() }",
    convert = r#"{ id }"#
)]
fn f(id: u32, #[cached_skip] tag: String) -> u32 {
    let _ = tag;
    id
}

fn main() {}
