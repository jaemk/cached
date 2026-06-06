use cached::macros::once;

#[once(result = true)]
fn load() -> Result<u64, String> {
    Ok(0)
}

fn main() {}
