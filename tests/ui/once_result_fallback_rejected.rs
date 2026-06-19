use cached::macros::once;

#[once(result_fallback = true)]
fn f() -> Result<i32, String> {
    Ok(42)
}

fn main() {}
