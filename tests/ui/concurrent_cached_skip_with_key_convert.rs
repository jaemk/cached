use cached::macros::concurrent_cached;

// A custom key is built only from `convert`, so `#[cached_skip]` would do nothing.
#[concurrent_cached(key = "u32", convert = r#"{ id }"#)]
fn f(id: u32, #[cached_skip] tag: String) -> u32 {
    let _ = tag;
    id
}

fn main() {}
