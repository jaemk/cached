use cached::macros::cached;

// A custom key is built only from `convert`, so `#[cached_skip]` would do nothing.
#[cached(key = "u32", convert = r#"{ id }"#)]
fn f(id: u32, #[cached_skip] tag: String) -> u32 {
    let _ = tag;
    id
}

fn main() {}
