use cached::concurrent_cached;

#[concurrent_cached]
async fn async_concurrent_without_feature() -> Result<i32, ()> {
    Ok(42)
}

fn main() {}
