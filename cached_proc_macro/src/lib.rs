use darling::FromMeta;
use proc_macro::TokenStream;
use quote::quote;
use syn::{
    parse_macro_input, parse_str, AttributeArgs, Block, FnArg, Ident, ItemFn, Pat, ReturnType, Type,
};

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
    key: Option<String>,
    #[darling(default)]
    convert: Option<String>,
    #[darling(default)]
    result: bool,
    #[darling(default)]
    option: bool,
    #[darling(default, rename = "type")]
    cache_type: Option<String>,
    #[darling(default, rename = "create")]
    cache_create: Option<String>,
}

/// # Attributes
/// - **Cache Name:** Use `name = "CACHE_NAME"` to specify the name for the generated cache.
/// - **Cache Type:** The default cache type is `UnboundCache`.
/// You specify which of the built-in cache types to use with `unbound`, `size = cache_size`, or `time = lifetime_in_seconds`
/// - **Cache Create:** You can specify the cache creation with `create = "{ CacheType::new() }"`.
/// - **Custom Cache Type:** You can use `type = "CacheType"` to specify the type of cache to use.
/// This requires create to also be set.
/// - **Cache Key:** Use `key = "KeyType"` to specify what type to use for the cache key.
/// This requires convert to also be set.
/// - **Cache Key Convert:** Use `convert = "{ convert_inputs_to_key }"`.
/// This requires either key or type to also be set.
/// - **Caching Result/Option:** If your function returns a `Result` or `Option`
/// you may want to use `result` or `option` to only cache when the output is `Ok` or `Some`
/// ## Note
/// The `type`, `create`, `key`, and `convert` attributes must be in a `String`
/// This is because darling, which is used for parsing the attributes, does not support parsing attributes into `Type` or `Block`.
#[proc_macro_attribute]
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
    let _attributes = input.attrs;
    let visibility = input.vis;
    let signature = input.sig;
    let body = input.block;

    // pull out the parts of the function signature
    let fn_ident = signature.ident.clone();
    let inputs = signature.inputs.clone();
    let output = signature.output.clone();
    let asyncness = signature.asyncness.clone();

    // pull out the names and types of the function inputs
    let input_tys = inputs
        .iter()
        .map(|input| match input {
            FnArg::Receiver(_) => panic!("methods (functions taking 'self') are not supported"),
            FnArg::Typed(pat_type) => pat_type.ty.clone(),
        })
        .collect::<Vec<Box<Type>>>();

    let input_names = inputs
        .iter()
        .map(|input| match input {
            FnArg::Receiver(_) => panic!("methods (functions taking 'self') are not supported"),
            FnArg::Typed(pat_type) => pat_type.pat.clone(),
        })
        .collect::<Vec<Box<Pat>>>();

    // pull out the output type
    let output_ty = match &output {
        ReturnType::Default => quote! {()},
        ReturnType::Type(_, ty) => quote! {#ty},
    };

    // make the cache identifier
    let cache_ident = match args.name {
        Some(name) => Ident::new(&name, fn_ident.span()),
        None => Ident::new(&fn_ident.to_string().to_uppercase(), fn_ident.span()),
    };

    // make the cache key type and block that converts the inputs into the key type
    let (cache_key_ty, key_convert_block) = match (&args.key, &args.convert, &args.cache_type) {
        (Some(key_str), Some(convert_str), _) => {
            let cache_key_ty = parse_str::<Type>(key_str).expect("unable to parse cache key type");

            let key_convert_block =
                parse_str::<Block>(convert_str).expect("unable to parse key convert block");

            (quote! {#cache_key_ty}, quote! {#key_convert_block})
        }
        (None, Some(convert_str), Some(_)) => {
            let key_convert_block =
                parse_str::<Block>(convert_str).expect("unable to parse key convert block");

            (quote! {}, quote! {#key_convert_block})
        }
        (None, None, _) => (
            quote! {(#(#input_tys),*)},
            quote! {(#(#input_names.clone()),*)},
        ),
        (Some(_), None, _) => panic!("key requires convert to be set"),
        (None, Some(_), None) => panic!("convert requires key or type to be set"),
    };

    // make the cache type and create statement
    let (cache_ty, cache_create) = match (
        &args.unbound,
        &args.size,
        &args.time,
        &args.cache_type,
        &args.cache_create,
    ) {
        (true, None, None, None, None) => {
            let cache_ty = quote! {cached::UnboundCache<#cache_key_ty, #output_ty>};
            let cache_create = quote! {cached::UnboundCache::new()};
            (cache_ty, cache_create)
        }
        (false, Some(size), None, None, None) => {
            let cache_ty = quote! {cached::SizedCache<#cache_key_ty, #output_ty>};
            let cache_create = quote! {cached::SizedCache::with_size(#size)};
            (cache_ty, cache_create)
        }
        (false, None, Some(time), None, None) => {
            let cache_ty = quote! {cached::TimedCache<#cache_key_ty, #output_ty>};
            let cache_create = quote! {cached::TimedCache::with_lifespan(#time)};
            (cache_ty, cache_create)
        }
        (false, None, None, None, None) => {
            let cache_ty = quote! {cached::UnboundCache<#cache_key_ty, #output_ty>};
            let cache_create = quote! {cached::UnboundCache::new()};
            (cache_ty, cache_create)
        }
        (false, None, None, Some(type_str), Some(create_str)) => {
            let cache_type = parse_str::<Type>(type_str).expect("unable to parse cache type");

            let cache_create =
                parse_str::<Block>(create_str).expect("unable to parse cache create block");

            (quote! { #cache_type }, quote! { #cache_create })
        }
        (false, None, None, Some(_), None) => panic!("type requires create to also be set"),
        (false, None, None, None, Some(_)) => panic!("create requires type to also be set"),
        _ => panic!("cache types (unbound, size, time, or type and create) are mutually exclusive"),
    };

    // make the set cache and return cache blocks
    let (set_cache_block, return_cache_block) = match (&args.result, &args.option) {
        (false, false) => {
            let set_cache_block = quote! { cache.cache_set(key, result.clone()); };
            let return_cache_block = quote! { return result.clone(); };
            (set_cache_block, return_cache_block)
        }
        (true, false) => {
            let set_cache_block = quote! {
                if let Ok(result) = &result {
                    cache.cache_set(key, Ok(result.clone()));
                }
            };
            let return_cache_block = quote! { return Ok(result.as_ref().unwrap().clone()); };
            (set_cache_block, return_cache_block)
        }
        (false, true) => {
            let set_cache_block = quote! {
                if let Some(result) = &result {
                    cache.cache_set(key, Some(result.clone()));
                }
            };
            let return_cache_block = quote! { return result.clone(); };
            (set_cache_block, return_cache_block)
        }
        _ => panic!("the result and option attributes are mutually exclusive"),
    };

    // put it all together
    let expanded = if asyncness.is_some() {
        quote! {
            #visibility static #cache_ident: ::cached::once_cell::sync::Lazy<::cached::async_std::sync::Mutex<#cache_ty>> = ::cached::once_cell::sync::Lazy::new(|| ::cached::async_std::sync::Mutex::new(#cache_create));
            #visibility #signature {
                use cached::Cached;
                let key = #key_convert_block;
                {
                    // check if the result is cached
                    let mut cache = #cache_ident.lock().await;
                    if let Some(result) = cache.cache_get(&key) {
                        #return_cache_block
                    }
                }

                // run the function and cache the result
                async fn inner(#inputs) #output #body;
                let result = inner(#(#input_names),*).await;

                let mut cache = #cache_ident.lock().await;
                #set_cache_block

                result
            }
        }
    } else {
        quote! {
            #visibility static #cache_ident: ::cached::once_cell::sync::Lazy<std::sync::Mutex<#cache_ty>> = ::cached::once_cell::sync::Lazy::new(|| std::sync::Mutex::new(#cache_create));
            #visibility #signature {
                use cached::Cached;
                let key = #key_convert_block;
                {
                    // check if the result is cached
                    let mut cache = #cache_ident.lock().unwrap();
                    if let Some(result) = cache.cache_get(&key) {
                        #return_cache_block
                    }
                }

                // run the function and cache the result
                fn inner(#inputs) #output #body;
                let result = inner(#(#input_names),*);

                let mut cache = #cache_ident.lock().unwrap();
                #set_cache_block

                result
            }
        }
    };

    expanded.into()
}
