
#[macro_export]
macro_rules! cached {
    ($cachename:ident ; $name:ident ($($arg:ident : $argtype:ty),*) -> $ret:ty ; $body:expr) => {
        lazy_static! {
            static ref $cachename: ::std::sync::Mutex<::std::collections::HashMap<($($argtype),*), $ret>> = {
                ::std::sync::Mutex::new(::std::collections::HashMap::new())
            };
        }
        pub fn $name($($arg: $argtype),*) -> $ret {
            let key = ($($arg.clone()),*);
            {
                let cache = $cachename.lock().unwrap();
                let res = cache.get(&key);
                if let Some(res) = res { return res.clone(); }
            }
            let val = $body;
            let mut cache = $cachename.lock().unwrap();
            cache.insert(key, val.clone());
            val
        }
    };
}


use std::collections::HashMap;
use std::hash::Hash;
use std::cmp::Eq;

#[macro_export]
macro_rules! cached_with {
    ($cached:ident ; $cachetype:ty ; $wrapped:ident ; $($arg:ident : $argtype:ty),* ; $ret:ty) => {
        pub struct $cached {
            pub cache: $cachetype,
        }
        impl $cached {
            pub fn new(cache: $cachetype) -> $cached {
                $cached { cache: cache }
            }
            pub fn call(&mut self, $($arg: $argtype),*) -> $ret {
                let key = ($($arg.clone()),*);
                {
                    let res = self.cache.get(&key);
                    if let Some(res) = res { println!("hit!"); return res.clone(); }
                }
                let val = $wrapped($($arg),*);
                self.cache.set(key, val.clone());
                val
            }
        }
    }
}


//pub trait Cached<K, V> {
//    fn get(&mut self, k: &K) -> Option<&V>;
//    fn set(&mut self, k: K, v: V);
//}


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
//}
//impl <K: Hash + Eq, V> Cached<K, V> for Cache<K, V> {
    pub fn get(&mut self, k: &K) -> Option<&V> {
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
    pub fn set(&mut self, k: K, v: V) {
        self.store.insert(k, v);
    }
}


#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
    }
}
