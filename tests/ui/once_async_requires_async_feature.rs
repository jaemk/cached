use cached::once;

#[once]
async fn async_once_without_feature() -> i32 {
    42
}

fn main() {}
