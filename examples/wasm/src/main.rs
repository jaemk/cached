/*
`cached` on `wasm32-unknown-unknown`: `cached::time` is backed by `web_time`,
so `#[cached]` / `TtlCache` work in the browser. This is a standalone workspace
crate (its Cargo.toml enables `cached` with `proc_macro,async,time_stores`),
not a `cargo run --example` target.

Build:
    cd examples/wasm && cargo build --target=wasm32-unknown-unknown
*/

use gloo_net::http::Request;
use yew::prelude::*;

use cached::TtlCache;
use cached::macros::cached;

const URL: &str = "https://echo.zuplo.io/";

#[derive(Clone, PartialEq)]
struct State {
    content: Option<String>,
    date: String,
}

fn now() -> String {
    js_sys::Date::new_0().to_iso_string().into()
}

#[component(App)]
fn app() -> Html {
    let state = use_state(|| State {
        content: None,
        date: now(),
    });
    let onclick = {
        let state = state.clone();
        Callback::from(move |_| {
            let state = state.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let content = fetch("Body content".to_owned()).await;
                state.set(State {
                    content,
                    date: now(),
                });
            })
        })
    };
    html! {
        <>
            <button {onclick}>{"Fetch Content"}</button><br/>
            <span>{"Last clicked: "}{state.date.clone()}</span><br/>
            <div>
                {state.content.clone().unwrap_or_else(|| "Click the button".to_owned())}
            </div>
        </>
    }
}

#[cached(
    ty = "TtlCache<String, Option<String>>",
    create = "{ TtlCache::builder().ttl(cached::time::Duration::from_secs(5)).build().unwrap() }",
    cache_none = true
)]
async fn fetch(body: String) -> Option<String> {
    Request::post(URL)
        .body(body)
        .ok()?
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()
}

fn main() {
    yew::Renderer::<App>::new().render();
}
