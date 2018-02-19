/*!
Macro for defining functions that wrap a static-ref cache object.
 */

#[macro_export]
macro_rules! cached {
    // Use default cached::Cache
    ($cachename:ident;
     fn $name:ident ($($arg:ident : $argtype:ty),*) -> $ret:ty = $body:expr) => {
        cached!(
            $cachename : $crate::UnboundCache<($($argtype),*), $ret> = $crate::UnboundCache::new();
            fn $name($($arg : $argtype),*) -> $ret = $body
        );
    };

    // Use a specified cache-type and an explicitly created cache-instance
    ($cachename:ident : $cachetype:ty = $cacheinstance:expr ;
     fn $name:ident ($($arg:ident : $argtype:ty),*) -> $ret:ty = $body:expr) => {
        lazy_static! {
            static ref $cachename: ::std::sync::Mutex<$cachetype> = {
                ::std::sync::Mutex::new($cacheinstance)
            };
        }
        #[allow(unused_parens)]
        pub fn $name($($arg: $argtype),*) -> $ret {
            let key = ($($arg.clone()),*);
            {
                let mut cache = $cachename.lock().unwrap();
                let res = $crate::Cached::cache_get(&mut *cache, &key);
                if let Some(res) = res { return res.clone(); }
            }
            let val = (||$body)();
            let mut cache = $cachename.lock().unwrap();
            $crate::Cached::cache_set(&mut *cache, key, val.clone());
            val
        }
    };
}

