use crate::helpers::*;
use darling::FromMeta;
use darling::ast::NestedMeta;
use proc_macro::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{Block, Ident, ItemFn, ReturnType, Type, parse_macro_input, parse_str};

#[derive(Debug, Default, Eq, PartialEq)]
enum SyncLock {
    #[default]
    Mutex,
    RwLock,
}

// Hand-written so both the documented `"rwlock"` spelling and darling's
// derived snake_case `"rw_lock"` are accepted (the derive only accepted the
// latter, so the publicly-documented `sync_lock = "rwlock"` failed to compile).
impl FromMeta for SyncLock {
    fn from_string(value: &str) -> darling::Result<Self> {
        match value {
            "mutex" => Ok(Self::Mutex),
            "rwlock" | "rw_lock" => Ok(Self::RwLock),
            _ => Err(darling::Error::unknown_value(value)),
        }
    }
}

#[derive(FromMeta)]
struct CachedMacroArgs {
    #[darling(default)]
    name: Option<String>,
    #[darling(default)]
    unbound: bool,
    /// Sets the maximum number of cached entries (the LRU bound).
    /// Mirrors the `max_size` builder/constructor naming on the cache stores.
    #[darling(default)]
    max_size: Option<usize>,
    #[darling(default)]
    ttl: Option<u64>,
    #[darling(default)]
    refresh: bool,
    #[darling(default)]
    time: Option<u64>,
    #[darling(default)]
    time_refresh: Option<bool>,
    /// Removed alias for `max_size`; kept only to emit a helpful rename error.
    #[darling(default)]
    size: Option<usize>,
    #[darling(default)]
    key: Option<String>,
    #[darling(default)]
    convert: Option<String>,
    #[darling(default)]
    cache_err: bool,
    #[darling(default)]
    cache_none: bool,
    #[darling(default)]
    sync_writes: SyncWriteMode,
    #[darling(default = "default_sync_writes_buckets")]
    sync_writes_buckets: usize,
    #[darling(default)]
    sync_lock: Option<SyncLock>,
    #[darling(default)]
    with_cached_flag: bool,
    #[darling(default)]
    ty: Option<String>,
    #[darling(default)]
    create: Option<String>,
    #[darling(default)]
    result_fallback: bool,
    #[darling(default)]
    unsync_reads: bool,
    #[darling(default)]
    expires: bool,
    // Removed attributes intercepted to provide helpful error messages
    #[darling(default)]
    result: Option<bool>,
    #[darling(default)]
    option: Option<bool>,
}

fn default_sync_writes_buckets() -> usize {
    64
}

pub fn cached(args: TokenStream, input: TokenStream) -> TokenStream {
    let attr_args = match NestedMeta::parse_meta_list(args.into()) {
        Ok(v) => v,
        Err(e) => {
            return TokenStream::from(darling::Error::from(e).write_errors());
        }
    };
    let args = match CachedMacroArgs::from_list(&attr_args) {
        Ok(v) => v,
        Err(e) => {
            return TokenStream::from(e.write_errors());
        }
    };
    let input = parse_macro_input!(input as ItemFn);

    // pull out the parts of the input
    let mut attributes = input.attrs;
    let visibility = input.vis;
    let signature = input.sig;
    let body = input.block;

    // pull out the parts of the function signature
    let fn_ident = signature.ident.clone();
    let inputs = signature.inputs.clone();
    let output = signature.output.clone();
    let asyncness = signature.asyncness;

    if inputs
        .iter()
        .any(|input| matches!(input, syn::FnArg::Receiver(_)))
    {
        return syn::Error::new(
            fn_ident.span(),
            "#[cached] cannot be applied to methods that take `self`",
        )
        .to_compile_error()
        .into();
    }

    // Reject zero `max_size`/`ttl` at expansion time (matching `#[concurrent_cached]`),
    // rather than letting the generated builder `build()` panic at first call.
    if matches!(args.max_size, Some(0)) {
        return syn::Error::new(fn_ident.span(), "`max_size` must be >= 1")
            .to_compile_error()
            .into();
    }
    if matches!(args.ttl, Some(0)) {
        return syn::Error::new(fn_ident.span(), "`ttl` must be >= 1")
            .to_compile_error()
            .into();
    }

    if args.time.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`time` was renamed to `ttl` in cached 1.0; use `ttl = ...`",
        )
        .to_compile_error()
        .into();
    }

    if args.time_refresh.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`time_refresh` was renamed to `refresh` in cached 1.0; use `refresh = ...`",
        )
        .to_compile_error()
        .into();
    }

    if args.size.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`size` was renamed to `max_size`; use `max_size = ...`",
        )
        .to_compile_error()
        .into();
    }

    if args.result.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "the `result` attribute has been removed. `Result<T, E>` returns now skip caching \
             `Err` by default. Remove `result = true` (or `result = false`), or use \
             `cache_err = true` to force-cache `Err` values.",
        )
        .to_compile_error()
        .into();
    }

    if args.option.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "the `option` attribute has been removed. `Option<T>` returns now skip caching \
             `None` by default. Remove `option = true` (or `option = false`), or use \
             `cache_none = true` to force-cache `None` values.",
        )
        .to_compile_error()
        .into();
    }

    if args.expires && args.ttl.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`expires` and `ttl` are mutually exclusive — \
             `expires` delegates expiry to the value via the `Expires` trait; \
             `ttl` applies a uniform time-based TTL to all entries",
        )
        .to_compile_error()
        .into();
    }

    if args.expires && args.ty.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`expires` and `ty` are mutually exclusive — \
             `expires` generates the store type automatically",
        )
        .to_compile_error()
        .into();
    }

    if args.expires && args.create.is_some() {
        return syn::Error::new(
            fn_ident.span(),
            "`expires` and `create` are mutually exclusive — \
             `expires` generates the store constructor automatically",
        )
        .to_compile_error()
        .into();
    }

    if args.expires && args.with_cached_flag {
        return syn::Error::new(
            fn_ident.span(),
            "`expires` and `with_cached_flag` are mutually exclusive — \
             the `Return<T>` wrapper does not implement `Expires`",
        )
        .to_compile_error()
        .into();
    }

    if args.expires && args.unsync_reads {
        return syn::Error::new(
            fn_ident.span(),
            "`expires` and `unsync_reads` are mutually exclusive — \
             `ExpiringCache` and `ExpiringLruCache` do not implement `CachedRead`",
        )
        .to_compile_error()
        .into();
    }

    if args.expires && args.refresh {
        return syn::Error::new(
            fn_ident.span(),
            "`expires` and `refresh` are mutually exclusive — \
             `refresh` renews a TTL on cache hit, but `ExpiringCache` and \
             `ExpiringLruCache` have no TTL to refresh; expiry is controlled by the value",
        )
        .to_compile_error()
        .into();
    }

    if args.expires && args.unbound {
        return syn::Error::new(
            fn_ident.span(),
            "`expires` and `unbound` are mutually exclusive — \
             `ExpiringCache` (the default store for `expires`) is already unbounded; \
             use `expires = true` alone for an unbounded expiring cache",
        )
        .to_compile_error()
        .into();
    }
    if args.expires && args.cache_none {
        return syn::Error::new(
            fn_ident.span(),
            "`expires = true` and `cache_none = true` are incompatible — `expires` requires \
             the cache value type to implement `Expires`, but `cache_none = true` stores \
             `Option<V>` as the value, which does not implement `Expires`. \
             Remove `cache_none = true` (None values are not cached by default with `expires = true`).",
        )
        .to_compile_error()
        .into();
    }
    if args.expires && args.cache_err {
        return syn::Error::new(
            fn_ident.span(),
            "`expires = true` and `cache_err = true` are incompatible — `expires` requires \
             the cache value type to implement `Expires`, but `cache_err = true` stores \
             `Result<V, E>` as the value, which does not implement `Expires`. \
             Remove `cache_err = true` (Err values are not cached by default with `expires = true`).",
        )
        .to_compile_error()
        .into();
    }

    let input_tys = get_input_types(&inputs);
    let input_names = get_input_names(&inputs);

    // pull out the output type
    let output_ty = match &output {
        ReturnType::Default => quote! {()},
        ReturnType::Type(_, ty) => quote! {#ty},
    };

    let output_span = output_ty.span();
    let output_ts = TokenStream::from(output_ty.clone());
    let output_type_display = output_ts.to_string().replace(' ', "");

    if check_with_cache_flag(args.with_cached_flag, &output) {
        return with_cache_flag_error(output_span, output_type_display);
    }

    let is_result_return = is_result_return_type(&output);
    let is_option_return = is_option_return_type(&output);

    if args.cache_err && !is_result_return {
        return syn::Error::new(
            fn_ident.span(),
            "`cache_err = true` requires the function to return `Result<T, E>`",
        )
        .to_compile_error()
        .into();
    }
    if args.cache_none && !is_option_return {
        return syn::Error::new(
            fn_ident.span(),
            "`cache_none = true` requires the function to return `Option<T>`",
        )
        .to_compile_error()
        .into();
    }
    if args.cache_err && args.result_fallback {
        return syn::Error::new(
            fn_ident.span(),
            "`cache_err` and `result_fallback` are mutually exclusive",
        )
        .to_compile_error()
        .into();
    }
    if args.cache_none && args.with_cached_flag {
        return syn::Error::new(
            fn_ident.span(),
            "`cache_none = true` and `with_cached_flag = true` are structurally incompatible \
             on `Option<T>` returns: `with_cached_flag` stores the inner `T` from `Return<T>` \
             while `cache_none = true` stores `Option<T>` as the cached value — the same \
             cache entry cannot hold both types. Use `with_cached_flag = true` alone (to get \
             cache-state flags; `None` is not cached by default), or use `cache_none = true` \
             alone (to force-cache `None` values).",
        )
        .to_compile_error()
        .into();
    }

    // `is_smart_result`: cache only Ok, skip Err (default for Result returns; opt out with cache_err)
    // `is_smart_option`: cache only Some, skip None (default for Option returns; opt out with cache_none)
    let is_smart_result = is_result_return && !args.cache_err;
    let is_smart_option = is_option_return && !args.cache_none;

    let cache_value_ty = match find_value_type(is_smart_result, is_smart_option, &output, output_ty)
    {
        Ok(value_ty) => value_ty,
        Err(e) => return e.to_compile_error().into(),
    };

    // make the cache identifier
    let cache_ident = match args.name {
        Some(ref name) => Ident::new(name, fn_ident.span()),
        None => Ident::new(&fn_ident.to_string().to_uppercase(), fn_ident.span()),
    };

    let (cache_key_ty, key_convert_block) =
        match make_cache_key_type(&args.key, &args.convert, &args.ty, input_tys, &input_names) {
            Ok(key) => key,
            Err(error) => return error.to_compile_error().into(),
        };

    // make the cache type and create statement
    let (cache_ty, cache_create) = if args.expires {
        if let Some(size) = args.max_size {
            (
                quote! { cached::ExpiringLruCache<#cache_key_ty, #cache_value_ty> },
                quote! { cached::ExpiringLruCache::builder().max_size(#size).build().unwrap_or_else(|e| panic!("ExpiringLruCache build failed in #[cached]: {e}")) },
            )
        } else {
            (
                quote! { cached::ExpiringCache<#cache_key_ty, #cache_value_ty> },
                quote! { cached::ExpiringCache::builder().build().unwrap_or_else(|e| panic!("ExpiringCache build failed in #[cached]: {e}")) },
            )
        }
    } else {
        match (
            &args.unbound,
            &args.max_size,
            &args.ttl,
            &args.ty,
            &args.create,
            &args.refresh,
        ) {
            (true, None, None, None, None, _) => {
                let cache_ty = quote! {cached::UnboundCache<#cache_key_ty, #cache_value_ty>};
                let cache_create = quote! {cached::UnboundCache::builder().build().unwrap_or_else(|e| panic!("UnboundCache build failed in #[cached]: {e}"))};
                (cache_ty, cache_create)
            }
            (false, Some(size), None, None, None, _) => {
                let cache_ty = quote! {cached::LruCache<#cache_key_ty, #cache_value_ty>};
                let cache_create = quote! {cached::LruCache::builder().max_size(#size).build().unwrap_or_else(|e| panic!("LruCache build failed in #[cached]: {e}"))};
                (cache_ty, cache_create)
            }
            (false, None, Some(ttl), None, None, refresh) => {
                let cache_ty = quote! {cached::TtlCache<#cache_key_ty, #cache_value_ty>};
                let cache_create = quote! {cached::TtlCache::builder().ttl(::cached::time::Duration::from_secs(#ttl)).refresh_on_hit(#refresh).build().unwrap_or_else(|e| panic!("TtlCache build failed in #[cached]: {e}"))};
                (cache_ty, cache_create)
            }
            (false, Some(size), Some(ttl), None, None, refresh) => {
                let cache_ty = quote! {cached::LruTtlCache<#cache_key_ty, #cache_value_ty>};
                let cache_create = quote! {cached::LruTtlCache::builder().max_size(#size).ttl(::cached::time::Duration::from_secs(#ttl)).refresh_on_hit(#refresh).build().unwrap_or_else(|e| panic!("LruTtlCache build failed in #[cached]: {e}"))};
                (cache_ty, cache_create)
            }
            (false, None, None, None, None, _) => {
                let cache_ty = quote! {cached::UnboundCache<#cache_key_ty, #cache_value_ty>};
                let cache_create = quote! {cached::UnboundCache::builder().build().unwrap_or_else(|e| panic!("UnboundCache build failed in #[cached]: {e}"))};
                (cache_ty, cache_create)
            }
            (false, None, None, Some(type_str), Some(create_str), _) => {
                let ty = match parse_str::<Type>(type_str) {
                    Ok(ty) => ty,
                    Err(error) => {
                        return syn::Error::new(
                            fn_ident.span(),
                            format!("unable to parse cache type: {error}"),
                        )
                        .to_compile_error()
                        .into();
                    }
                };

                let cache_create = match parse_str::<Block>(create_str) {
                    Ok(block) => block,
                    Err(error) => {
                        return syn::Error::new(
                            fn_ident.span(),
                            format!("unable to parse cache create block: {error}"),
                        )
                        .to_compile_error()
                        .into();
                    }
                };

                (quote! { #ty }, quote! { #cache_create })
            }
            (false, None, None, Some(_), None, _) => {
                return syn::Error::new(fn_ident.span(), "`ty` requires `create` to also be set")
                    .to_compile_error()
                    .into();
            }
            (false, None, None, None, Some(_), _) => {
                return syn::Error::new(fn_ident.span(), "`create` requires `ty` to also be set")
                    .to_compile_error()
                    .into();
            }
            _ => {
                return syn::Error::new(
                fn_ident.span(),
                "cache types (`unbound`, `max_size` and/or `ttl`, or `ty` and `create`) are mutually exclusive",
            )
            .to_compile_error()
            .into();
            }
        }
    };

    // make the set cache and return cache blocks
    let (set_cache_block, return_cache_block) = match (is_smart_result, is_smart_option) {
        (false, false) => {
            let set_cache_block = quote! { cache.set(key, result.clone()); };
            let return_cache_block = if args.with_cached_flag {
                quote! { let mut r = result.to_owned(); r.was_cached = true; return r }
            } else {
                quote! { return result.to_owned() }
            };
            (set_cache_block, return_cache_block)
        }
        (true, false) => {
            let set_cache_block = quote! {
                if let Ok(result) = &result {
                    cache.set(key, result.clone());
                }
            };
            let return_cache_block = if args.with_cached_flag {
                quote! { let mut r = result.to_owned(); r.was_cached = true; return Ok(r) }
            } else {
                quote! { return Ok(result.to_owned()) }
            };
            (set_cache_block, return_cache_block)
        }
        (false, true) => {
            let set_cache_block = quote! {
                if let Some(result) = &result {
                    cache.set(key, result.clone());
                }
            };
            let return_cache_block = if args.with_cached_flag {
                quote! { let mut r = result.to_owned(); r.was_cached = true; return Some(r) }
            } else {
                quote! { return Some(result.clone()) }
            };
            (set_cache_block, return_cache_block)
        }
        (true, true) => unreachable!("return type cannot be both Result and Option"),
    };

    if let Err(error) = validate_sync_writes_buckets(args.sync_writes_buckets, fn_ident.span()) {
        return error.to_compile_error().into();
    }

    if args.result_fallback && args.sync_writes != SyncWriteMode::Disabled {
        return syn::Error::new(
            fn_ident.span(),
            "`result_fallback` and `sync_writes` are mutually exclusive",
        )
        .to_compile_error()
        .into();
    }

    if args.result_fallback && !is_result_return {
        return syn::Error::new(
            fn_ident.span(),
            "`result_fallback` requires a `Result<T, E>` return type",
        )
        .to_compile_error()
        .into();
    }

    if args.result_fallback && args.ty.is_none() && args.ttl.is_none() && !args.expires {
        return syn::Error::new(
            fn_ident.span(),
            "`result_fallback` requires a store that implements `CloneCached`. \
             The default `UnboundCache` and `LruCache` (size without ttl) do not implement it. \
             Use `ttl` (for `TtlCache`), `max_size` + `ttl` (for `LruTtlCache`), \
             `expires` (for `ExpiringCache`/`ExpiringLruCache`), or a custom `ty`.",
        )
        .to_compile_error()
        .into();
    }

    if args.unsync_reads && matches!(args.sync_lock, Some(SyncLock::Mutex)) {
        return syn::Error::new(
            fn_ident.span(),
            "`unsync_reads` requires an RwLock; remove `sync_lock = \"mutex\"`",
        )
        .to_compile_error()
        .into();
    }

    if args.unsync_reads && args.ty.is_none() && (args.max_size.is_some() || args.ttl.is_some()) {
        return syn::Error::new(
            fn_ident.span(),
            "`unsync_reads` requires a store that implements `CachedRead` (no mutation on reads). \
             `LruCache` and `LruTtlCache` update LRU recency on reads; `TtlCache` may refresh TTL. \
             Use the default store (UnboundCache), `TtlSortedCache`, or a custom `ty` that implements `CachedRead`.",
        )
        .to_compile_error()
        .into();
    }

    let sync_writes_buckets = args.sync_writes_buckets;

    let set_cache_and_return = quote! {
        #set_cache_block
        result
    };

    let use_rwlock = match args.sync_lock {
        Some(SyncLock::RwLock) => true,
        Some(SyncLock::Mutex) => false,
        None => true, // Default to RwLock for all caches now that traits support &self reads
    };

    let lock_type = if use_rwlock {
        if asyncness.is_some() {
            quote! { ::cached::async_sync::RwLock }
        } else {
            quote! { ::cached::sync_sync::RwLock }
        }
    } else {
        if asyncness.is_some() {
            quote! { ::cached::async_sync::Mutex }
        } else {
            quote! { ::cached::sync_sync::Mutex }
        }
    };

    let lock_method = if use_rwlock {
        quote! { write }
    } else {
        quote! { lock }
    };
    let read_lock_method = if use_rwlock {
        quote! { read }
    } else {
        quote! { lock }
    };
    let await_if_async = if asyncness.is_some() {
        quote! { .await }
    } else {
        quote! {}
    };

    let no_cache_fn_ident = Ident::new(&format!("{}_no_cache", &fn_ident), fn_ident.span());

    // Build the origin ("no cache") function by cloning the full original
    // signature and renaming it. Quoting the whole `syn::Signature` (rather
    // than rebuilding it as `#generics (#inputs) #output`) preserves the
    // `where` clause, lifetimes, const generics, and `const`/`unsafe`/`abi`:
    // `#generics` alone emits only the angle-bracketed params and silently
    // drops the where clause.
    let mut no_cache_sig = signature.clone();
    no_cache_sig.ident = no_cache_fn_ident.clone();

    let function_no_cache;
    let function_call;
    let ty;
    if asyncness.is_some() {
        function_no_cache = quote! {
            #no_cache_sig #body
        };

        function_call = quote! {
            let result = #no_cache_fn_ident(#(#input_names),*).await;
        };

        ty = match args.sync_writes {
            SyncWriteMode::ByKey => quote! {
                #visibility static #cache_ident: ::std::sync::LazyLock<(#lock_type<#cache_ty>, Vec<std::sync::Arc<#lock_type<()>>>)> = ::std::sync::LazyLock::new(|| (#lock_type::new(#cache_create), (0..#sync_writes_buckets).map(|_| std::sync::Arc::new(#lock_type::new(()))).collect()));
            },
            _ => quote! {
                #visibility static #cache_ident: ::std::sync::LazyLock<#lock_type<#cache_ty>> = ::std::sync::LazyLock::new(|| #lock_type::new(#cache_create));
            },
        };
    } else {
        function_no_cache = quote! {
            #no_cache_sig #body
        };

        function_call = quote! {
            let result = #no_cache_fn_ident(#(#input_names),*);
        };

        ty = match args.sync_writes {
            SyncWriteMode::ByKey => quote! {
                #visibility static #cache_ident: ::std::sync::LazyLock<(#lock_type<#cache_ty>, Vec<std::sync::Arc<#lock_type<()>>>)> = ::std::sync::LazyLock::new(|| (#lock_type::new(#cache_create), (0..#sync_writes_buckets).map(|_| std::sync::Arc::new(#lock_type::new(()))).collect()));
            },
            _ => quote! {
                #visibility static #cache_ident: ::std::sync::LazyLock<#lock_type<#cache_ty>> = ::std::sync::LazyLock::new(|| #lock_type::new(#cache_create));
            },
        };
    }

    let (lock, do_set_return_block) = {
        let lock = match args.sync_writes {
            SyncWriteMode::ByKey => {
                let key_lock_block = by_key_lock_block(
                    quote! { key },
                    quote! { locks },
                    lock_method.clone(),
                    await_if_async.clone(),
                );
                quote! {
                    let (cache_mutex, locks) = &*#cache_ident;
                    #key_lock_block
                    let mut cache = cache_mutex.#lock_method()#await_if_async;
                }
            }
            _ => quote! {
                let mut cache = #cache_ident.#lock_method()#await_if_async;
            },
        };

        let cache_get_return_block = if args.unsync_reads {
            quote! {
                let cache = #cache_ident.#read_lock_method()#await_if_async;
                if let Some(result) = ::cached::CachedRead::cache_get_read(&*cache, &key) {
                    #return_cache_block
                }
            }
        } else {
            quote! {
                let mut cache = #cache_ident.#lock_method()#await_if_async;
                if let Some(result) = cache.cache_get(&key) {
                    #return_cache_block
                }
            }
        };

        let default_unsync_cache_get_return_block = quote! {
            let cache = #cache_ident.#read_lock_method()#await_if_async;
            if ::cached::CachedPeek::cache_peek(&*cache, &key).is_some() {
                if let Some(result) = ::cached::CachedRead::cache_get_read(&*cache, &key) {
                    #return_cache_block
                }
            }
        };

        let by_key_cache_get_return_block = if args.unsync_reads {
            quote! {
                let cache = cache_mutex.#read_lock_method()#await_if_async;
                if let Some(result) = ::cached::CachedRead::cache_get_read(&*cache, &key) {
                    #return_cache_block
                }
            }
        } else {
            quote! {
                let mut cache = cache_mutex.#lock_method()#await_if_async;
                if let Some(result) = cache.cache_get(&key) {
                    #return_cache_block
                }
            }
        };

        let do_set_return_block = match args.sync_writes {
            SyncWriteMode::ByKey => {
                let key_lock_block = by_key_lock_block(
                    quote! { key },
                    quote! { locks },
                    lock_method.clone(),
                    await_if_async.clone(),
                );
                quote! {
                    let (cache_mutex, locks) = &*#cache_ident;
                    #key_lock_block
                    {
                        #by_key_cache_get_return_block
                    }
                    #function_call
                    let mut cache = cache_mutex.#lock_method()#await_if_async;
                    #set_cache_and_return
                }
            }
            SyncWriteMode::Default => {
                if args.unsync_reads {
                    quote! {
                        {
                            #default_unsync_cache_get_return_block
                        }
                        let mut cache = #cache_ident.#lock_method()#await_if_async;
                        if let Some(result) = cache.cache_get(&key) {
                            #return_cache_block
                        }
                        #function_call
                        #set_cache_and_return
                    }
                } else {
                    quote! {
                        let mut cache = #cache_ident.#lock_method()#await_if_async;
                        if let Some(result) = cache.cache_get(&key) {
                            #return_cache_block
                        }
                        #function_call
                        #set_cache_and_return
                    }
                }
            }
            SyncWriteMode::Disabled => {
                if args.result_fallback {
                    quote! {
                        let old_val = {
                            let mut cache = #cache_ident.#lock_method()#await_if_async;
                            let (result, has_expired) = cache.cache_get_with_expiry_status(&key);
                            if let (Some(result), false) = (&result, has_expired) {
                                #return_cache_block
                            }
                            result
                        };
                        #function_call
                        let mut cache = #cache_ident.#lock_method()#await_if_async;
                        let result = match (result.is_err(), old_val) {
                            (true, Some(old_val)) => {
                                Ok(old_val)
                            }
                            _ => result
                        };
                        #set_cache_and_return
                    }
                } else {
                    quote! {
                        {
                            #cache_get_return_block
                        }
                        #function_call
                        let mut cache = #cache_ident.#lock_method()#await_if_async;
                        #set_cache_and_return
                    }
                }
            }
        };
        (lock, do_set_return_block)
    };

    let signature_no_muts = get_mut_signature(signature);

    // create a signature for the cache-priming function
    let prime_fn_ident = Ident::new(&format!("{}_prime_cache", &fn_ident), fn_ident.span());
    let mut prime_sig = signature_no_muts.clone();
    prime_sig.ident = prime_fn_ident;

    // make cached static, cached function and prime cached function doc comments
    let cache_ident_doc = format!("Cached static for the [`{}`] function.", fn_ident);
    let no_cache_fn_indent_doc = format!("Origin of the cached function [`{}`].", fn_ident);
    let prime_fn_indent_doc = format!("Primes the cached function [`{}`].", fn_ident);
    let cache_fn_doc_extra = format!(
        "This is a cached function that uses the [`{}`] cached static.",
        cache_ident
    );
    fill_in_attributes(&mut attributes, cache_fn_doc_extra);

    let prime_do_set_return_block = quote! {
        // try to get a lock first
        #lock
        // run the function and cache the result
        #function_call
        #set_cache_and_return
    };

    // put it all together
    let expanded = quote! {
        // Cached static
        #[doc = #cache_ident_doc]
        #ty
        // No cache function (origin of the cached function)
        #[doc = #no_cache_fn_indent_doc]
        #visibility #function_no_cache
        // Cached function
        #(#attributes)*
        #visibility #signature_no_muts {
            use cached::Cached;
            use cached::CloneCached;
            let key = #key_convert_block;
            #do_set_return_block
        }
        // Prime cached function
        #[doc = #prime_fn_indent_doc]
        #[allow(dead_code)]
        #(#attributes)*
        #visibility #prime_sig {
            use cached::Cached;
            let key = #key_convert_block;
            #prime_do_set_return_block
        }
    };

    expanded.into()
}
