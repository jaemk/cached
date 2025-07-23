use crate::helpers::*;
use attrs::*;
use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::{parse::Parser as _, spanned::Spanned as _};
use syn::{
    parse_macro_input, parse_str, Block, Expr, ExprClosure, GenericArgument, Ident, ItemFn,
    PathArguments, ReturnType, Signature, Type,
};

pub fn io_cached(args: TokenStream, input: TokenStream) -> TokenStream {
    let mut map_error = None::<String>;
    let mut time = None::<u64>;

    let mut time_refresh = None::<bool>;
    let mut sync_to_disk_on_cache_change = None::<bool>;

    let mut with_cached_flag = false;
    let mut disk = false;
    let mut redis = false;

    let mut disk_dir = None::<String>;
    let mut cache_prefix_block = None::<String>;
    let mut name = None::<String>;
    let mut key = None::<String>;
    let mut convert = None::<String>;
    let mut ty = None::<String>;
    let mut create = None::<String>;
    let mut connection_config = None::<String>;

    match Attrs::new()
        .once("map_error", with::eq(set::lit(&mut map_error)))
        .once("time", with::eq(set::lit(&mut time)))
        .once("time_refresh", with::eq(set::lit(&mut time_refresh)))
        .once(
            "sync_to_disk_on_cache_change",
            with::eq(set::lit(&mut sync_to_disk_on_cache_change)),
        )
        .once("with_cached_flag", with::eq(on::lit(&mut with_cached_flag)))
        .once("disk", with::eq(on::lit(&mut disk)))
        .once("redis", with::eq(on::lit(&mut redis)))
        .once("disk_dir", with::eq(set::lit(&mut disk_dir)))
        .once(
            "cache_prefix_block",
            with::eq(set::lit(&mut cache_prefix_block)),
        )
        .once("name", with::eq(set::lit(&mut name)))
        .once("key", with::eq(set::lit(&mut key)))
        .once("convert", with::eq(set::lit(&mut convert)))
        .once("ty", with::eq(set::lit(&mut ty)))
        .once("create", with::eq(set::lit(&mut create)))
        .once(
            "connection_config",
            with::eq(set::lit(&mut connection_config)),
        )
        .parse(args)
    {
        Ok(()) => {}
        Err(e) => return e.into_compile_error().into(),
    }
    let Some(map_error) = map_error else {
        return syn::Error::new(Span::call_site(), "Argument `map_error` is required")
            .into_compile_error()
            .into();
    };

    let ItemFn {
        attrs: mut attributes,
        vis: visibility,
        sig: signature,
        block: body,
    } = parse_macro_input!(input as _);

    let signature_no_muts = get_mut_signature(signature.clone());

    let Signature {
        ident: fn_ident,
        inputs,
        output,
        asyncness,
        ..
    } = signature;

    let input_tys = get_input_types(&inputs);

    let input_names = get_input_names(&inputs);

    // pull out the output type
    let output_ty = match &output {
        ReturnType::Default => quote! {()},
        ReturnType::Type(_, ty) => quote! {#ty},
    };

    let output_span = output_ty.span();
    let output_ts = TokenStream::from(output_ty);
    let output_parts = get_output_parts(&output_ts);
    let output_string = output_parts.join("::");
    let output_type_display = output_ts.to_string().replace(' ', "");

    // if `with_cached_flag = true`, then enforce that the return type
    // is something wrapped in `Return`. Either `Return<T>` or the
    // fully qualified `cached::Return<T>`
    if with_cached_flag
        && !output_string.contains("Return")
        && !output_string.contains("cached::Return")
    {
        return syn::Error::new(
            output_span,
            format!(
                "\nWhen specifying `with_cached_flag = true`, \
                    the return type must be wrapped in `cached::Return<T>`. \n\
                    The following return types are supported: \n\
                    |    `Result<cached::Return<T>, E>`\n\
                    Found type: {t}.",
                t = output_type_display
            ),
        )
        .to_compile_error()
        .into();
    }

    // Find the type of the value to store.
    // Return type always needs to be a result, so we want the (first) inner type.
    // For Result<i32, String>, store i32, etc.
    let cache_value_ty = match output.clone() {
        ReturnType::Default => {
            panic!(
                "#[io_cached] functions must return `Result`s, found {:?}",
                output_type_display
            );
        }
        ReturnType::Type(_, ty) => {
            if let Type::Path(typepath) = *ty {
                let segments = typepath.path.segments;
                if let PathArguments::AngleBracketed(brackets) = &segments.last().unwrap().arguments
                {
                    let inner_ty = brackets.args.first().unwrap();
                    if output_string.contains("Return") || output_string.contains("cached::Return")
                    {
                        if let GenericArgument::Type(Type::Path(typepath)) = inner_ty {
                            let segments = &typepath.path.segments;
                            if let PathArguments::AngleBracketed(brackets) =
                                &segments.last().unwrap().arguments
                            {
                                let inner_ty = brackets.args.first().unwrap();
                                quote! {#inner_ty}
                            } else {
                                panic!(
                                    "#[io_cached] unable to determine cache value type, found {:?}",
                                    output_type_display
                                );
                            }
                        } else {
                            panic!(
                                "#[io_cached] unable to determine cache value type, found {:?}",
                                output_type_display
                            );
                        }
                    } else {
                        quote! {#inner_ty}
                    }
                } else {
                    panic!("#[io_cached] functions must return `Result`s")
                }
            } else {
                panic!(
                    "function return type too complex, #[io_cached] functions must return `Result`s"
                )
            }
        }
    };

    // make the cache identifier
    let cache_ident = match name {
        Some(ref name) => Ident::new(name, fn_ident.span()),
        None => Ident::new(&fn_ident.to_string().to_uppercase(), fn_ident.span()),
    };
    let cache_name = cache_ident.to_string();

    let (cache_key_ty, key_convert_block) =
        make_cache_key_type(&key, &convert, &ty, input_tys, &input_names);

    // make the cache type and create statement
    let (cache_ty, cache_create) = match (
        &redis,
        &disk,
        &time,
        &time_refresh,
        &cache_prefix_block,
        &ty,
        &create,
        &sync_to_disk_on_cache_change,
        &connection_config,
    ) {
        // redis
        (true, false, time, time_refresh, cache_prefix, ty, cache_create, _, _) => {
            let cache_ty = match ty {
                Some(ty) => {
                    let ty = parse_str::<Type>(ty).expect("unable to parse cache type");
                    quote! { #ty }
                }
                None => {
                    if asyncness.is_some() {
                        quote! { cached::AsyncRedisCache<#cache_key_ty, #cache_value_ty> }
                    } else {
                        quote! { cached::RedisCache<#cache_key_ty, #cache_value_ty> }
                    }
                }
            };
            let cache_create = match cache_create {
                Some(cache_create) => {
                    if time.is_some() || time_refresh.is_some() || cache_prefix.is_some() {
                        panic!("cannot specify `time`, `time_refresh`, or `cache_prefix` when passing `create block");
                    } else {
                        let cache_create = parse_str::<Block>(cache_create.as_ref())
                            .expect("unable to parse cache create block");
                        quote! { #cache_create }
                    }
                }
                None => {
                    if time.is_none() {
                        if asyncness.is_some() {
                            panic!("AsyncRedisCache requires a `time` when `create` block is not specified")
                        } else {
                            panic!(
                                "RedisCache requires a `time` when `create` block is not specified"
                            )
                        };
                    } else {
                        let cache_prefix = if let Some(cp) = cache_prefix {
                            cp.to_string()
                        } else {
                            format!(" {{ \"cached::proc_macro::io_cached::{}\" }}", cache_ident)
                        };
                        let cache_prefix = parse_str::<Block>(cache_prefix.as_ref())
                            .expect("unable to parse cache_prefix_block");
                        match time_refresh {
                            Some(time_refresh) => {
                                if asyncness.is_some() {
                                    quote! { cached::AsyncRedisCache::new(#cache_prefix, Duration::from_secs(#time)).set_refresh(#time_refresh).build().await.expect("error constructing AsyncRedisCache in #[io_cached] macro") }
                                } else {
                                    quote! {
                                        cached::RedisCache::new(#cache_prefix, Duration::from_secs(#time)).set_refresh(#time_refresh).build().expect("error constructing RedisCache in #[io_cached] macro")
                                    }
                                }
                            }
                            None => {
                                if asyncness.is_some() {
                                    quote! { cached::AsyncRedisCache::new(#cache_prefix, Duration::from_secs(#time)).build().await.expect("error constructing AsyncRedisCache in #[io_cached] macro") }
                                } else {
                                    quote! {
                                        cached::RedisCache::new(#cache_prefix, Duration::from_secs(#time)).build().expect("error constructing RedisCache in #[io_cached] macro")
                                    }
                                }
                            }
                        }
                    }
                }
            };
            (cache_ty, cache_create)
        }
        // disk
        (
            false,
            true,
            time,
            time_refresh,
            _,
            ty,
            cache_create,
            sync_to_disk_on_cache_change,
            connection_config,
        ) => {
            let cache_ty = match ty {
                Some(ty) => {
                    let ty = parse_str::<Type>(ty).expect("unable to parse cache type");
                    quote! { #ty }
                }
                None => {
                    // https://github.com/spacejam/sled?tab=readme-ov-file#interaction-with-async
                    quote! { cached::DiskCache<#cache_key_ty, #cache_value_ty> }
                }
            };
            let connection_config = match connection_config {
                Some(connection_config) => {
                    let connection_config = parse_str::<Expr>(connection_config)
                        .expect("unable to parse connection_config block");
                    Some(quote! { #connection_config })
                }
                None => None,
            };
            let cache_create = match cache_create {
                Some(cache_create) => {
                    if time.is_some() || time_refresh.is_some() {
                        panic!(
                            "cannot specify `time` or `time_refresh` when passing `create block"
                        );
                    } else {
                        let cache_create = parse_str::<Block>(cache_create.as_ref())
                            .expect("unable to parse cache create block");
                        quote! { #cache_create }
                    }
                }
                None => {
                    let create = quote! {
                        cached::DiskCache::new(#cache_name)
                    };
                    let create = match time {
                        None => create,
                        Some(time) => {
                            quote! {
                                (#create).set_lifespan(Duration::from_secs(#time))
                            }
                        }
                    };
                    let create = match time_refresh {
                        None => create,
                        Some(time_refresh) => {
                            quote! {
                                (#create).set_refresh(#time_refresh)
                            }
                        }
                    };
                    let create = match sync_to_disk_on_cache_change {
                        None => create,
                        Some(sync_to_disk_on_cache_change) => {
                            quote! {
                                (#create).set_sync_to_disk_on_cache_change(#sync_to_disk_on_cache_change)
                            }
                        }
                    };
                    let create = match connection_config {
                        None => create,
                        Some(connection_config) => {
                            quote! {
                                (#create).set_connection_config(#connection_config)
                            }
                        }
                    };
                    let create = match disk_dir {
                        None => create,
                        Some(disk_dir) => {
                            quote! { (#create).set_disk_directory(#disk_dir) }
                        }
                    };
                    quote! { (#create).build().expect("error constructing DiskCache in #[io_cached] macro") }
                }
            };
            (cache_ty, cache_create)
        }
        (_, _, time, time_refresh, cache_prefix, ty, cache_create, _, _) => {
            let cache_ty = match ty {
                Some(ty) => {
                    let ty = parse_str::<Type>(ty).expect("unable to parse cache type");
                    quote! { #ty }
                }
                None => panic!("#[io_cached] cache `ty` must be specified"),
            };
            let cache_create = match cache_create {
                Some(cache_create) => {
                    if time.is_some() || time_refresh.is_some() || cache_prefix.is_some() {
                        panic!("cannot specify `time`, `time_refresh`, or `cache_prefix` when passing `create block");
                    } else {
                        let cache_create = parse_str::<Block>(cache_create.as_ref())
                            .expect("unable to parse cache create block");
                        quote! { #cache_create }
                    }
                }
                None => {
                    panic!("#[io_cached] cache `create` block must be specified");
                }
            };
            (cache_ty, cache_create)
        }
        #[allow(unreachable_patterns)]
        _ => panic!("#[io_cached] cache types cache type could not be determined"),
    };

    let map_error = &map_error;
    let map_error = parse_str::<ExprClosure>(map_error).expect("unable to parse map_error block");

    // make the set cache and return cache blocks
    let (set_cache_block, return_cache_block) = {
        let (set_cache_block, return_cache_block) = if with_cached_flag {
            (
                if asyncness.is_some() && !disk {
                    quote! {
                        if let Ok(result) = &result {
                            cache.cache_set(key, result.value.clone()).await.map_err(#map_error)?;
                        }
                    }
                } else {
                    quote! {
                        if let Ok(result) = &result {
                            cache.cache_set(key, result.value.clone()).map_err(#map_error)?;
                        }
                    }
                },
                quote! { let mut r = ::cached::Return::new(result.clone()); r.was_cached = true; return Ok(r) },
            )
        } else {
            (
                if asyncness.is_some() && !disk {
                    quote! {
                        if let Ok(result) = &result {
                            cache.cache_set(key, result.clone()).await.map_err(#map_error)?;
                        }
                    }
                } else {
                    quote! {
                        if let Ok(result) = &result {
                            cache.cache_set(key, result.clone()).map_err(#map_error)?;
                        }
                    }
                },
                quote! { return Ok(result.clone()) },
            )
        };
        (set_cache_block, return_cache_block)
    };

    let do_set_return_block = if asyncness.is_some() {
        quote! {
            // run the function and cache the result
            async fn inner(#inputs) #output #body;
            let result = inner(#(#input_names),*).await;
            let cache = &#cache_ident.get_or_init(init).await;
            #set_cache_block
            result
        }
    } else {
        quote! {
            // run the function and cache the result
            fn inner(#inputs) #output #body;
            let result = inner(#(#input_names),*);
            let cache = &#cache_ident;
            #set_cache_block
            result
        }
    };

    // create a signature for the cache-priming function
    let prime_fn_ident = Ident::new(&format!("{}_prime_cache", &fn_ident), fn_ident.span());
    let mut prime_sig = signature_no_muts.clone();
    prime_sig.ident = prime_fn_ident;

    // make cached static, cached function and prime cached function doc comments
    let cache_ident_doc = format!("Cached static for the [`{}`] function.", fn_ident);
    let prime_fn_indent_doc = format!("Primes the cached function [`{}`].", fn_ident);
    let cache_fn_doc_extra = format!(
        "This is a cached function that uses the [`{}`] cached static.",
        cache_ident
    );
    fill_in_attributes(&mut attributes, cache_fn_doc_extra);

    let async_trait = if asyncness.is_some() && !disk {
        quote! {
            use cached::IOCachedAsync;
        }
    } else {
        quote! {
            use cached::IOCached;
        }
    };

    let async_cache_get_return = if asyncness.is_some() && !disk {
        quote! {
            if let Some(result) = cache.cache_get(&key).await.map_err(#map_error)? {
                #return_cache_block
            }
        }
    } else {
        quote! {
            if let Some(result) = cache.cache_get(&key).map_err(#map_error)? {
                #return_cache_block
            }
        }
    };
    // put it all together
    let expanded = if asyncness.is_some() {
        quote! {
            // Cached static
            #[doc = #cache_ident_doc]
            #visibility static #cache_ident: ::cached::async_sync::OnceCell<#cache_ty> = ::cached::async_sync::OnceCell::const_new();
            // Cached function
            #(#attributes)*
            #visibility #signature_no_muts {
                let init = || async { #cache_create };
                #async_trait
                let key = #key_convert_block;
                {
                    // check if the result is cached
                    let cache = &#cache_ident.get_or_init(init).await;
                    #async_cache_get_return
                }
                #do_set_return_block
            }
            // Prime cached function
            #[doc = #prime_fn_indent_doc]
            #[allow(dead_code)]
            #visibility #prime_sig {
                #async_trait
                let init = || async { #cache_create };
                let key = #key_convert_block;
                #do_set_return_block
            }
        }
    } else {
        quote! {
            // Cached static
            #[doc = #cache_ident_doc]
            #visibility static #cache_ident: ::cached::once_cell::sync::Lazy<#cache_ty> = ::cached::once_cell::sync::Lazy::new(|| #cache_create);
            // Cached function
            #(#attributes)*
            #visibility #signature_no_muts {
                use cached::IOCached;
                let key = #key_convert_block;
                {
                    // check if the result is cached
                    let cache = &#cache_ident;
                    if let Some(result) = cache.cache_get(&key).map_err(#map_error)? {
                        #return_cache_block
                    }
                }
                #do_set_return_block
            }
            // Prime cached function
            #[doc = #prime_fn_indent_doc]
            #[allow(dead_code)]
            #visibility #prime_sig {
                use cached::IOCached;
                let key = #key_convert_block;
                #do_set_return_block
            }
        }
    };

    expanded.into()
}
