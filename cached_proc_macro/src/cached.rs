use crate::helpers::*;
use darling::FromMeta;
use proc_macro::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{parse_macro_input, parse_str, AttributeArgs, Block, Ident, ItemFn, ReturnType, Type};

#[derive(FromMeta)]
struct MacroArgs {
    #[darling(default)]
    name: Option<String>,
    #[darling(default)]
    unbound: bool,
    #[darling(default)]
    size: Option<usize>,
    #[darling(default)]
    time: Option<u64>,
    #[darling(default)]
    time_refresh: bool,
    #[darling(default)]
    key: Option<String>,
    #[darling(default)]
    convert: Option<String>,
    #[darling(default)]
    result: bool,
    #[darling(default)]
    option: bool,
    #[darling(default)]
    sync_writes: bool,
    #[darling(default)]
    with_cached_flag: bool,
    #[darling(default, rename = "type")]
    cache_type: Option<String>,
    #[darling(default, rename = "create")]
    cache_create: Option<String>,
}

pub fn cached(args: TokenStream, input: TokenStream) -> TokenStream {
    let attr_args = parse_macro_input!(args as AttributeArgs);
    let args = match MacroArgs::from_list(&attr_args) {
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

    let input_tys = get_input_types(&inputs);
    let input_names = get_input_names(&inputs);

    // pull out the output type
    let output_ty = match &output {
        ReturnType::Default => quote! {()},
        ReturnType::Type(_, ty) => quote! {#ty},
    };

    let output_span = output_ty.span();
    let output_ts = TokenStream::from(output_ty.clone());
    let output_parts = get_output_parts(&output_ts);
    let output_string = output_parts.join("::");
    let output_type_display = output_ts.to_string().replace(' ', "");

    if check_with_cache_flag(args.with_cached_flag, output_string) {
        return with_cache_flag_error(output_span, output_type_display);
    }

    let cache_value_ty = find_value_type(args.result, args.option, &output, output_ty);

    // make the cache identifier
    let cache_ident = match args.name {
        Some(ref name) => Ident::new(name, fn_ident.span()),
        None => Ident::new(&fn_ident.to_string().to_uppercase(), fn_ident.span()),
    };

    let (cache_key_ty, key_convert_block) = make_cache_key_type(
        &args.key,
        &args.convert,
        &args.cache_type,
        input_tys,
        &input_names,
    );

    // make the cache type and create statement
    let (cache_ty, cache_create) = match (
        &args.unbound,
        &args.size,
        &args.time,
        &args.cache_type,
        &args.cache_create,
        &args.time_refresh,
    ) {
        (true, None, None, None, None, _) => {
            let cache_ty = quote! {cached::UnboundCache<#cache_key_ty, #cache_value_ty>};
            let cache_create = quote! {cached::UnboundCache::new()};
            (cache_ty, cache_create)
        }
        (false, Some(size), None, None, None, _) => {
            let cache_ty = quote! {cached::SizedCache<#cache_key_ty, #cache_value_ty>};
            let cache_create = quote! {cached::SizedCache::with_size(#size)};
            (cache_ty, cache_create)
        }
        (false, None, Some(time), None, None, time_refresh) => {
            let cache_ty = quote! {cached::TimedCache<#cache_key_ty, #cache_value_ty>};
            let cache_create =
                quote! {cached::TimedCache::with_lifespan_and_refresh(#time, #time_refresh)};
            (cache_ty, cache_create)
        }
        (false, Some(size), Some(time), None, None, time_refresh) => {
            let cache_ty = quote! {cached::TimedSizedCache<#cache_key_ty, #cache_value_ty>};
            let cache_create = quote! {cached::TimedSizedCache::with_size_and_lifespan_and_refresh(#size, #time, #time_refresh)};
            (cache_ty, cache_create)
        }
        (false, None, None, None, None, _) => {
            let cache_ty = quote! {cached::UnboundCache<#cache_key_ty, #cache_value_ty>};
            let cache_create = quote! {cached::UnboundCache::new()};
            (cache_ty, cache_create)
        }
        (false, None, None, Some(type_str), Some(create_str), _) => {
            let cache_type = parse_str::<Type>(type_str).expect("unable to parse cache type");

            let cache_create =
                parse_str::<Block>(create_str).expect("unable to parse cache create block");

            (quote! { #cache_type }, quote! { #cache_create })
        }
        (false, None, None, Some(_), None, _) => {
            panic!("type requires create to also be set")
        }
        (false, None, None, None, Some(_), _) => {
            panic!("create requires type to also be set")
        }
        _ => panic!(
            "cache types (unbound, size and/or time, or type and create) are mutually exclusive"
        ),
    };

    // make the set cache and return cache blocks
    let (set_cache_block, return_cache_block) = match (&args.result, &args.option) {
        (false, false) => {
            let set_cache_block = quote! { cache.cache_set(key, result.clone()); };
            let return_cache_block = if args.with_cached_flag {
                quote! { let mut r = result.clone(); r.was_cached = true; return r }
            } else {
                quote! { return result.clone() }
            };
            (set_cache_block, return_cache_block)
        }
        (true, false) => {
            let set_cache_block = quote! {
                if let Ok(result) = &result {
                    cache.cache_set(key, result.clone());
                }
            };
            let return_cache_block = if args.with_cached_flag {
                quote! { let mut r = result.clone(); r.was_cached = true; return Ok(r) }
            } else {
                quote! { return Ok(result.clone()) }
            };
            (set_cache_block, return_cache_block)
        }
        (false, true) => {
            let set_cache_block = quote! {
                if let Some(result) = &result {
                    cache.cache_set(key, result.clone());
                }
            };
            let return_cache_block = if args.with_cached_flag {
                quote! { let mut r = result.clone(); r.was_cached = true; return Some(r) }
            } else {
                quote! { return Some(result.clone()) }
            };
            (set_cache_block, return_cache_block)
        }
        _ => panic!("the result and option attributes are mutually exclusive"),
    };

    let set_cache_and_return = quote! {
        #set_cache_block
        result
    };
    let lock;
    let function_call;
    let cache_type;
    if asyncness.is_some() {
        lock = quote! {
            // try to get a lock first
            let mut cache = #cache_ident.lock().await;
        };

        function_call = quote! {
            // run the function and cache the result
            async fn inner(#inputs) #output #body;
            let result = inner(#(#input_names),*).await;
        };

        cache_type = quote! {
            #visibility static #cache_ident: ::cached::once_cell::sync::Lazy<::cached::async_sync::Mutex<#cache_ty>> = ::cached::once_cell::sync::Lazy::new(|| ::cached::async_sync::Mutex::new(#cache_create));
        };
    } else {
        lock = quote! {
            // try to get a lock first
            let mut cache = #cache_ident.lock().unwrap();
        };

        function_call = quote! {
            // run the function and cache the result
            fn inner(#inputs) #output #body;
            let result = inner(#(#input_names),*);
        };

        cache_type = quote! {
            #visibility static #cache_ident: ::cached::once_cell::sync::Lazy<std::sync::Mutex<#cache_ty>> = ::cached::once_cell::sync::Lazy::new(|| std::sync::Mutex::new(#cache_create));
        };
    }

    let prime_do_set_return_block = quote! {
        #lock
        #function_call
        #set_cache_and_return
    };

    let do_set_return_block = if args.sync_writes {
        quote! {
            #lock
            if let Some(result) = cache.cache_get(&key) {
                #return_cache_block
            }
            #function_call
            #set_cache_and_return
        }
    } else {
        quote! {
            {
                #lock
                if let Some(result) = cache.cache_get(&key) {
                    #return_cache_block
                }
            }
            #function_call
            #lock
            #set_cache_and_return
        }
    };

    let signature_no_muts = get_mut_signature(signature);

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

    // put it all together
    let expanded = quote! {
        // Cached static
        #[doc = #cache_ident_doc]
        #cache_type
        // Cached function
        #(#attributes)*
        #visibility #signature_no_muts {
            use cached::Cached;
            let key = #key_convert_block;
            #do_set_return_block
        }
        // Prime cached function
        #[doc = #prime_fn_indent_doc]
        #[allow(dead_code)]
        #visibility #prime_sig {
            use cached::Cached;
            let key = #key_convert_block;
            #prime_do_set_return_block
        }
    };

    expanded.into()
}
