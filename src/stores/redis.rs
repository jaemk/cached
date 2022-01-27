use super::Cached;
use redis::{Client, Commands, RedisResult};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::env;
use std::fmt::Display;
use std::marker::PhantomData;
use std::time::{SystemTime, UNIX_EPOCH};
#[cfg(feature = "async")]
use {super::CachedAsync, async_trait::async_trait, futures::Future};

/// Cache store optionally bound by time
///
/// Values can be timestamped when inserted,
/// then will be evicted if expired at time of retrieval.
#[derive(Debug)]
pub struct RedisCache<K, V> {
    pub(super) store: Vec<V>,
    pub(super) seconds: Option<u64>,
    pub(super) hits: u64,
    pub(super) misses: u64,
    client: Client,
    prefix: String,
    _phantom: PhantomData<K>,
}

const ENV_KEY: &str = "REDIS_CS";
const PREFIX: &str = "cached_key_prefix-";

impl<K, V> RedisCache<K, V>
where
    K: Display,
    V: Serialize + DeserializeOwned + Clone,
{
    /// Creates an empty `RedisCache` definition
    pub fn new() -> Self {
        Self {
            store: vec![],
            seconds: None,
            hits: 0,
            misses: 0,
            client: Self::get_client(None),
            prefix: Self::generate_prefix(),
            _phantom: Default::default(),
        }
    }

    /// Creates a new `RedisCache` with a specified lifespan
    pub fn with_lifespan(seconds: u64) -> Self {
        Self {
            store: vec![],
            seconds: Some(seconds),
            hits: 0,
            misses: 0,
            client: Self::get_client(None),
            prefix: Self::generate_prefix(),
            _phantom: Default::default(),
        }
    }

    /// Set the prefix for the keys
    pub fn set_prefix(mut self, prefix: &str) -> Self {
        self.prefix = prefix.to_string();
        self
    }

    /// Set the connection string for redis
    pub fn set_connection_string(mut self, cs: &str) -> Self {
        self.client = Self::get_client(cs.to_string());
        self
    }

    fn get_client(cs: impl Into<Option<String>>) -> Client {
        let cs = cs.into().unwrap_or_else(|| {
            env::var(ENV_KEY).unwrap_or_else(|_| {
                panic!(
                    "Environment variable for Redis connection string is missing, please set {} env var.",
                    ENV_KEY
                )
            })
        }
        );

        redis::Client::open(cs).expect("Cannot connect to Redis")
    }

    fn generate_prefix() -> String {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string();
        format!("{}{}-", PREFIX, timestamp)
    }

    fn generate_key(&self, key: &K) -> String {
        format!("{}{}", self.prefix, key)
    }

    fn clean_up(&mut self) {
        self.store.clear();
    }

    fn base_vec_fill(&mut self, val: &str) -> &mut V {
        let index = self.store.len();
        self.store.push(serde_json::from_str(val).unwrap());
        &mut self.store[index]
    }

    fn base_get(&self, key: K) -> Option<String> {
        let mut con = self.client.get_connection().unwrap();
        let val: RedisResult<String> = con.get(self.generate_key(&key));
        val.ok()
    }

    fn base_set(&self, key: K, val: V) {
        let mut con = self.client.get_connection().unwrap();
        let generated_key = self.generate_key(&key);

        match self.seconds {
            None => con
                .set::<String, String, String>(generated_key, serde_json::to_string(&val).unwrap())
                .unwrap(),
            Some(s) => con
                .set_ex::<String, String, String>(
                    generated_key,
                    serde_json::to_string(&val).unwrap(),
                    s as usize,
                )
                .unwrap(),
        };
    }
}

impl<K, V> Default for RedisCache<K, V>
where
    K: Display,
    V: Serialize + DeserializeOwned + Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<'de, K, V> Cached<K, V> for RedisCache<K, V>
where
    K: Display + Clone,
    V: Serialize + DeserializeOwned + Clone,
{
    fn cache_get(&mut self, key: &K) -> Option<&V> {
        self.clean_up();

        let val = self.base_get(key.clone());

        match val {
            Some(val) => {
                self.hits += 1;
                Some(self.base_vec_fill(&val))
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }

    fn cache_get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.clean_up();

        let val = self.base_get(key.clone());

        match val {
            Some(val) => {
                self.hits += 1;
                Some(self.base_vec_fill(&val))
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }

    fn cache_set(&mut self, key: K, val: V) -> Option<V> {
        self.clean_up();
        let old_val = self
            .base_get(key.clone())
            .map(|val| serde_json::from_str(&val).unwrap());
        self.base_set(key, val);
        old_val
    }

    fn cache_get_or_set_with<F: FnOnce() -> V>(&mut self, key: K, f: F) -> &mut V {
        self.clean_up();

        let val = self.base_get(key.clone());

        match val {
            Some(val) => {
                self.hits += 1;
                self.base_vec_fill(&val)
            }
            None => {
                self.misses += 1;
                let val = f();
                self.base_set(key, val.clone());
                let index = self.store.len();
                self.store.push(val);
                &mut self.store[index]
            }
        }
    }

    fn cache_remove(&mut self, key: &K) -> Option<V> {
        self.clean_up();
        let mut con = self.client.get_connection().unwrap();
        self.base_get(key.clone()).map(|val| {
            con.del::<String, ()>(self.generate_key(key)).unwrap();
            serde_json::from_str(&val).unwrap()
        })
    }

    fn cache_clear(&mut self) {
        // copied from https://stackoverflow.com/questions/4006324/how-to-atomically-delete-keys-matching-a-pattern-using-redis#comment39607023_16974060
        const REDIS_LUA_BATCH_DELETE_CMD: &str = "local keys = redis.call('keys', ARGV[1]) \n for i=1,#keys,5000 do \n redis.call('del', unpack(keys, i, math.min(i+4999, #keys))) \n end \n return keys";

        self.clean_up();
        let mut con = self.client.get_connection().unwrap();
        redis::cmd("EVAL")
            .arg(REDIS_LUA_BATCH_DELETE_CMD)
            .arg(0)
            .arg(format!("{}*", self.prefix))
            .query(&mut con)
            .unwrap()
    }

    /// In `RedisCache`, it's an alias to `cache_clear`
    fn cache_reset(&mut self) {
        self.cache_clear();
    }

    fn cache_size(&self) -> usize {
        let mut con = self.client.get_connection().unwrap();
        redis::cmd("EVAL")
            .arg(format!("return #redis.call('keys', '{}*')", self.prefix))
            .arg(0)
            .query(&mut con)
            .unwrap()
    }

    fn cache_hits(&self) -> Option<u64> {
        Some(self.hits)
    }

    fn cache_misses(&self) -> Option<u64> {
        Some(self.misses)
    }

    fn cache_lifespan(&self) -> Option<u64> {
        self.seconds
    }

    fn cache_set_lifespan(&mut self, seconds: u64) -> Option<u64> {
        self.clean_up();
        let old = self.seconds;
        self.seconds = Some(seconds);
        old
    }
}

#[cfg(feature = "async")]
#[async_trait]
impl<K, V> CachedAsync<K, V> for RedisCache<K, V>
where
    K: Send + Display,
    V: Serialize + DeserializeOwned + Clone,
{
    async fn get_or_set_with<F, Fut>(&mut self, key: K, f: F) -> &mut V
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = V> + Send,
    {
        let mut con = self.client.get_connection().unwrap();
        let generated_key = self.generate_key(&key);
        let val: RedisResult<String> = con.get(generated_key.clone());
        match val {
            Ok(val) => {
                self.hits += 1;
                self.base_vec_fill(&val)
            }
            Err(_) => {
                let val = f().await;
                self.base_set(key, val.clone());
                let index = self.store.len();
                self.store.push(val);
                &mut self.store[index]
            }
        }
    }

    async fn try_get_or_set_with<F, Fut, E>(&mut self, key: K, f: F) -> Result<&mut V, E>
    where
        V: Send,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<V, E>> + Send,
    {
        let mut con = self.client.get_connection().unwrap();
        let generated_key = self.generate_key(&key);
        let val: RedisResult<String> = con.get(generated_key.clone());
        let v = match val {
            Ok(val) => {
                self.hits += 1;
                self.base_vec_fill(&val)
            }
            Err(_) => {
                let val = f().await?;
                self.base_set(key, val.clone());
                let index = self.store.len();
                self.store.push(val);
                &mut self.store[index]
            }
        };

        Ok(v)
    }
}

#[cfg(test)]
/// Cache store tests
mod tests {
    use std::thread::sleep;
    use std::time::Duration;

    use super::*;

    #[test]
    fn redis_cache() {
        let mut c = RedisCache::with_lifespan(2);

        assert!(c.cache_get(&1).is_none());
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, misses);

        assert_eq!(c.cache_set(1, 100), None);
        assert!(c.cache_get(&1).is_some());
        let hits = c.cache_hits().unwrap();
        let misses = c.cache_misses().unwrap();
        assert_eq!(1, hits);
        assert_eq!(1, misses);

        sleep(Duration::new(2, 0));
        assert!(c.cache_get(&1).is_none());
        let misses = c.cache_misses().unwrap();
        assert_eq!(2, misses);

        let old = c.cache_set_lifespan(1).unwrap();
        assert_eq!(2, old);
        assert_eq!(c.cache_set(1, 100), None);
        assert!(c.cache_get(&1).is_some());
        let hits = c.cache_hits().unwrap();
        let misses = c.cache_misses().unwrap();
        assert_eq!(2, hits);
        assert_eq!(2, misses);

        sleep(Duration::new(1, 0));
        assert!(c.cache_get(&1).is_none());
        let misses = c.cache_misses().unwrap();
        assert_eq!(3, misses);

        c.cache_clear();
        c.cache_set_lifespan(10).unwrap();
        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 100), None);
        assert_eq!(c.store.len(), 0);
        assert_eq!(c.cache_get(&1), Some(&100));
        assert_eq!(c.store.len(), 1);
        assert_eq!(c.cache_get(&1), Some(&100));
        assert_eq!(c.store.len(), 1);
        c.cache_clear();
    }

    #[test]
    fn clear() {
        let mut c = RedisCache::with_lifespan(3600);

        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);
        c.cache_clear();

        assert_eq!(0, c.cache_size());
        c.cache_clear();
    }

    #[test]
    fn reset() {
        let mut c = RedisCache::with_lifespan(100);

        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);
        assert_eq!(0, c.store.capacity());

        c.cache_reset();

        assert_eq!(0, c.store.capacity());
        c.cache_clear();
    }

    #[test]
    fn remove() {
        let mut c = RedisCache::with_lifespan(3600);

        assert_eq!(c.cache_set(1, 100), None);
        assert_eq!(c.cache_set(2, 200), None);
        assert_eq!(c.cache_set(3, 300), None);

        assert_eq!(Some(100), c.cache_remove(&1));
        assert_eq!(2, c.cache_size());
        c.cache_clear();
    }

    #[test]
    fn get_or_set_with() {
        let mut c = RedisCache::with_lifespan(2);

        assert_eq!(c.cache_get_or_set_with(0, || 0), &0);
        assert_eq!(c.cache_get_or_set_with(1, || 1), &1);
        assert_eq!(c.cache_get_or_set_with(2, || 2), &2);
        assert_eq!(c.cache_get_or_set_with(3, || 3), &3);
        assert_eq!(c.cache_get_or_set_with(4, || 4), &4);
        assert_eq!(c.cache_get_or_set_with(5, || 5), &5);

        assert_eq!(c.cache_misses(), Some(6));

        assert_eq!(c.cache_get_or_set_with(0, || 0), &0);

        assert_eq!(c.cache_misses(), Some(6));

        assert_eq!(c.cache_get_or_set_with(0, || 42), &0);

        assert_eq!(c.cache_misses(), Some(6));

        sleep(Duration::new(2, 0));

        assert_eq!(c.cache_get_or_set_with(1, || 42), &42);

        assert_eq!(c.cache_misses(), Some(7));
        c.cache_clear();
    }
}
