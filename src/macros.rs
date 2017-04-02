/*!
Macro(s) for defining functions that wrap a static-ref cache object.

 */

#[macro_export]
/// Creates a function wrapping a cache.
/// There are several options for specifying a cache-type.
/// 1.) Use the default unbounded cache
/// ```rust,ignore
/// cached!{CACHE_NAME >>
/// func_name(arg1: arg1_type, arg2: arg2_type) -> return_type = {
///     <regular function body>
/// }}
///
/// 2.) Use an explicitly specified cache-type, but let the macro instantiate it.
///     The cache-type is expected to have a `new` method that takes no arguments.
/// ```rust,ignore
/// cached!{CACHE_NAME: SpecificCacheType >>
/// func_name(arg1: arg1_type, arg2: arg2_type) -> return_type = {
///     <regular function body>
/// }}
///
/// 3.) Use an explicitly specified cache-type and provide the instantiated cache struct.
///     This would allow using caches that require args in their constructor.
/// ```rust,ignore
/// cached!{CACHE_NAME: MyCache = MyCache::new(arg); >>
/// func_name(arg1: arg1_type, arg2: arg2_type) -> return_type = {
///     <regular function body>
/// }}
/// ```
macro_rules! cached {
    // Use default cached::Cache
    ($cachename:ident >> $name:ident ($($arg:ident : $argtype:ty),*) -> $ret:ty = $body:expr) => {
        lazy_static! {
            static ref $cachename: ::std::sync::Mutex<cached::Cache<($($argtype),*), $ret>> = {
                ::std::sync::Mutex::new(cached::Cache::new())
            };
        }
        #[allow(unused_parens)]
        pub fn $name($($arg: $argtype),*) -> $ret {
            let key = ($($arg.clone()),*);
            {
                let mut cache = $cachename.lock().unwrap();
                let res = cache.cache_get(&key);
                if let Some(res) = res { return res.clone(); }
            }
            let val = (||$body)();
            let mut cache = $cachename.lock().unwrap();
            cache.cache_set(key, val.clone());
            val
        }
    };

    // Use specified cache-type, implicitly create the cache (expect there to be a `new` method)
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
                cached::enforce_cached_impl(&cache);
                let res = cache.cache_get(&key);
                if let Some(res) = res { return res.clone(); }
            }
            let val = (||$body)();
            let mut cache = $cachename.lock().unwrap();
            cache.cache_set(key, val.clone());
            val
        }
    };

    // Use a specified cache-type and an explicitly created cache-instance
    ($cachename:ident : $cachetype:ident = $cacheinstance:expr ; >> $name:ident ($($arg:ident : $argtype:ty),*) -> $ret:ty = $body:expr) => {
        lazy_static! {
            static ref $cachename: ::std::sync::Mutex<$cachetype<($($argtype),*), $ret>> = {
                ::std::sync::Mutex::new($cacheinstance)
            };
        }
        #[allow(unused_parens)]
        pub fn $name($($arg: $argtype),*) -> $ret {
            let key = ($($arg.clone()),*);
            {
                let mut cache = $cachename.lock().unwrap();
                cached::enforce_cached_impl(&cache);
                let res = cache.cache_get(&key);
                if let Some(res) = res { return res.clone(); }
            }
            let val = (||$body)();
            let mut cache = $cachename.lock().unwrap();
            cache.cache_set(key, val.clone());
            val
        }
    };
}

