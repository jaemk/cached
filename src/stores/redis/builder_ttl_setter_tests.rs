// No Redis server needed -- these only inspect the builder's ttl field set by
// the convenience setters, without calling `build()`.
use super::RedisCacheBuilder;
use crate::time::Duration;

#[test]
fn ttl_secs_and_ttl_millis_set_duration() {
    let b = RedisCacheBuilder::<String, String>::new()
        .prefix("p")
        .ttl_secs(7);
    assert_eq!(b.ttl, Some(Duration::from_secs(7)));

    let b = RedisCacheBuilder::<String, String>::new()
        .prefix("p")
        .ttl_millis(250);
    assert_eq!(b.ttl, Some(Duration::from_millis(250)));
}

#[test]
fn ttl_setters_override_last_writer_wins() {
    // ttl(secs=10) then ttl_secs(5) -> 5s
    let b = RedisCacheBuilder::<String, String>::new()
        .prefix("p")
        .ttl(Duration::from_secs(10))
        .ttl_secs(5);
    assert_eq!(b.ttl, Some(Duration::from_secs(5)));

    // ttl_secs then ttl_millis -> the millis value
    let b = RedisCacheBuilder::<String, String>::new()
        .prefix("p")
        .ttl_secs(10)
        .ttl_millis(500);
    assert_eq!(b.ttl, Some(Duration::from_millis(500)));

    // ttl_millis then ttl -> the ttl value
    let b = RedisCacheBuilder::<String, String>::new()
        .prefix("p")
        .ttl_millis(500)
        .ttl(Duration::from_secs(3));
    assert_eq!(b.ttl, Some(Duration::from_secs(3)));
}
