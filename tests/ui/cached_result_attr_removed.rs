use cached::macros::cached;

#[cached(result = true)]
fn load(id: u64) -> Result<u64, String> {
    Ok(id)
}

fn main() {}
