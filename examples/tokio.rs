use cached::proc_macro::cached;
use std::time::Duration;
use tokio::time::sleep;

async fn sleep_secs(secs: u64) {
    sleep(Duration::from_secs(secs)).await;
}

#[cached]
async fn cached_sleep_secs(secs: u64) {
    sleep(Duration::from_secs(secs)).await;
}

#[tokio::main]
async fn main() {
    println!("sleeping for 4 seconds");
    sleep_secs(4).await;
    println!("sleeping for 4 seconds");
    sleep_secs(4).await;
    println!("cached sleeping for 4 seconds");
    cached_sleep_secs(4).await;
    println!("cached sleeping for 4 seconds");
    cached_sleep_secs(4).await;
}
