use async_std::task::sleep;
use cached::proc_macro::cached;
use std::time::Duration;

async fn sleep_secs(secs: u64) {
    sleep(Duration::from_secs(secs)).await;
}

#[cached]
async fn cached_sleep_secs(secs: u64) {
    sleep(Duration::from_secs(secs)).await;
}

#[cached(time = 1, key = "bool", convert = r#"{ true }"#, result = true)]
async fn only_cached_the_first_time(
    s: String,
) -> std::result::Result<Vec<String>, &'static dyn std::error::Error> {
    Ok(vec![s])
}

#[async_std::main]
async fn main() {
    let a = only_cached_the_first_time("a".to_string()).await.unwrap();
    let b = only_cached_the_first_time("b".to_string()).await.unwrap();
    assert_eq!(a, b);
    sleep_secs(1).await;
    let b = only_cached_the_first_time("b".to_string()).await.unwrap();
    assert_ne!(a, b);

    println!("sleeping for 4 seconds");
    sleep_secs(4).await;
    println!("sleeping for 4 seconds");
    sleep_secs(4).await;
    println!("cached sleeping for 4 seconds");
    cached_sleep_secs(4).await;
    println!("cached sleeping for 4 seconds");
    cached_sleep_secs(4).await;
    println!("done!");
}
