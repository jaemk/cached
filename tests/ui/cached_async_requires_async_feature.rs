use cached::cached;

#[cached]
async fn async_cached_without_feature() -> i32 {
    42
}

fn main() {}
