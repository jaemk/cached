use std::collections::HashMap;
use std::hash::Hash;
use std::cmp::Eq;


#[macro_export]
/// Creates a function wrapping a cache. `SpecificCacheType` is optional.
/// If `SpecificCacheType` is not provided, a default cache (`cached::Cache`) will be used.
/// `SpecificCacheType` must implement `cached::Cached`
///
/// Example:
/// ```rust,ignore
/// cached!{CACHE_NAME: SpecificCacheType >>
/// func_name(arg1: arg1_type, arg2: arg2_type) -> return_type = {
///     <regular function body>
/// }}
/// ```
macro_rules! cached {
    ($cachename:ident >> $name:ident ($($arg:ident : $argtype:ty),*) -> $ret:ty = $body:expr) => {
        lazy_static! {
            static ref $cachename: ::std::sync::Mutex<Cache<($($argtype),*), $ret>> = {
                ::std::sync::Mutex::new(Cache::new())
            };
        }
        #[allow(unused_parens)]
        pub fn $name($($arg: $argtype),*) -> $ret {
            let key = ($($arg.clone()),*);
            {
                let mut cache = $cachename.lock().unwrap();
                let res = cache.get(&key);
                if let Some(res) = res { return res.clone(); }
            }
            let val = (||$body)();
            let mut cache = $cachename.lock().unwrap();
            cache.set(key, val.clone());
            val
        }
    };
    ($cachename:ident : $cachetype:ident >> $name:ident ($($arg:ident : $argtype:ty),*) -> $ret:ty = $body:expr) => {
        lazy_static! {
            static ref $cachename: ::std::sync::Mutex<$cachetype<($($argtype),*), $ret>> = {
                ::std::sync::Mutex::new($cachetype::new())
            };
        }
        #[allow(unused_parens)]
        pub fn $name($($arg: $argtype),*) -> $ret {
            let key = ($($arg.clone()),*);
            {
                let mut cache = $cachename.lock().unwrap();
                let res = cache.get(&key);
                if let Some(res) = res { return res.clone(); }
            }
            let val = (||$body)();
            let mut cache = $cachename.lock().unwrap();
            cache.set(key, val.clone());
            val
        }
    };
}


pub trait Cached<K, V> {
    fn get(&mut self, k: &K) -> Option<&V>;
    fn set(&mut self, k: K, v: V);
    fn size(&self) -> usize;
    fn hits(&self) -> Option<u32> { None }
    fn misses(&self) -> Option<u32> { None }
    fn capacity(&self) -> Option<u32> { None }
    fn seconds(&self) -> Option<u64> { None }
}


pub struct Cache<K: Hash + Eq, V> {
    store: HashMap<K, V>,
    hits: u32,
    misses: u32,
}
impl <K: Hash + Eq, V> Cache<K, V> {
    pub fn new() -> Cache<K, V> {
        let store = HashMap::new();
        Cache {
            store: store,
            hits: 0,
            misses: 0,
        }
    }
}
impl <K: Hash + Eq, V> Cached<K, V> for Cache<K, V> {
    fn get(&mut self, k: &K) -> Option<&V> {
        match self.store.get(k) {
            Some(v) => {
                self.hits += 1;
                Some(v)
            }
            None =>  {
                self.misses += 1;
                None
            }
        }
    }
    fn set(&mut self, k: K, v: V) {
        self.store.insert(k, v);
    }
    fn size(&self) -> usize { self.store.len() }
    fn hits(&self) -> Option<u32> { Some(self.hits) }
    fn misses(&self) -> Option<u32> { Some(self.misses) }
}


#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
    }
}
