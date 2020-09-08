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
        static $cachename: $crate::once_cell::sync::Lazy<::std::sync::Mutex<$cachetype>>
            = $crate::once_cell::sync::Lazy::new(|| ::std::sync::Mutex::new($cacheinstance));

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

#[macro_export]
macro_rules! cached_key {
    // Use a specified cache-type and an explicitly created cache-instance
    ($cachename:ident : $cachetype:ty = $cacheinstance:expr ;
     Key = $key:expr;
     fn $name:ident ($($arg:ident : $argtype:ty),*) -> $ret:ty = $body:expr) => {
        static $cachename: $crate::once_cell::sync::Lazy<::std::sync::Mutex<$cachetype>>
            = $crate::once_cell::sync::Lazy::new(|| ::std::sync::Mutex::new($cacheinstance));

        #[allow(unused_parens)]
        pub fn $name($($arg: $argtype),*) -> $ret {
            let key = $key;
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

#[macro_export]
macro_rules! cached_result {
    // Unfortunately it's impossible to infer the cache type because it's not the function return type
    ($cachename:ident : $cachetype:ty = $cacheinstance:expr ;
     fn $name:ident ($($arg:ident : $argtype:ty),*) -> $ret:ty = $body:expr) => {
        static $cachename: $crate::once_cell::sync::Lazy<::std::sync::Mutex<$cachetype>>
            = $crate::once_cell::sync::Lazy::new(|| ::std::sync::Mutex::new($cacheinstance));

        #[allow(unused_parens)]
        pub fn $name($($arg: $argtype),*) -> $ret {
            let key = ($($arg.clone()),*);
            {
                let mut cache = $cachename.lock().unwrap();
                let res = $crate::Cached::cache_get(&mut *cache, &key);
                if let Some(res) = res { return Ok(res.clone()); }
            }

            // Store return in temporary typed variable in case type cannot be inferred
            let ret : $ret = (||$body)();
            let val = ret?;

            let mut cache = $cachename.lock().unwrap();
            $crate::Cached::cache_set(&mut *cache, key, val.clone());
            Ok(val)
        }
    };
}

#[macro_export]
macro_rules! cached_key_result {
    // Use a specified cache-type and an explicitly created cache-instance
    ($cachename:ident : $cachetype:ty = $cacheinstance:expr ;
     Key = $key:expr;
     fn $name:ident ($($arg:ident : $argtype:ty),*) -> $ret:ty = $body:expr) => {
        static $cachename: $crate::once_cell::sync::Lazy<::std::sync::Mutex<$cachetype>>
            = $crate::once_cell::sync::Lazy::new(|| ::std::sync::Mutex::new($cacheinstance));

        #[allow(unused_parens)]
        pub fn $name($($arg: $argtype),*) -> $ret {
            let key = $key;
            {
                let mut cache = $cachename.lock().unwrap();
                let res = $crate::Cached::cache_get(&mut *cache, &key);
                if let Some(res) = res { return Ok(res.clone()); }
            }

            // Store return in temporary typed variable in case type cannot be inferred
            let ret : $ret = (||$body)();
            let val = ret?;

            let mut cache = $cachename.lock().unwrap();
            $crate::Cached::cache_set(&mut *cache, key, val.clone());
            Ok(val)
        }
    };
}

#[macro_export]
macro_rules! cached_control {
    // Use a specified cache-type and an explicitly created cache-instance
    ($cachename:ident : $cachetype:ty = $cacheinstance:expr ;
     Key = $key:expr;
     PostGet($cached_value:ident) = $post_get:expr;
     PostExec($body_value:ident) = $post_exec:expr;
     Set($set_value:ident) = $pre_set:expr;
     Return($ret_value:ident) = $return:expr;
     fn $name:ident ($($arg:ident : $argtype:ty),*) -> $ret:ty = $body:expr) => {
        static $cachename: $crate::once_cell::sync::Lazy<::std::sync::Mutex<$cachetype>>
            = $crate::once_cell::sync::Lazy::new(|| ::std::sync::Mutex::new($cacheinstance));

        #[allow(unused_parens)]
        pub fn $name($($arg: $argtype),*) -> $ret {
            let key = $key;
            {
                let mut cache = $cachename.lock().unwrap();
                let res = $crate::Cached::cache_get(&mut *cache, &key);
                if let Some($cached_value) = res {
                    $post_get
                }
            }
            let $body_value = (||$body)();
            let $set_value = $post_exec;
            let mut cache = $cachename.lock().unwrap();
            $crate::Cached::cache_set(&mut *cache, key, $pre_set);
            let $ret_value = $set_value;
            $return
        }
    };
}
