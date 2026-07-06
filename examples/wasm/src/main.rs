/*
`cached` on `wasm32-unknown-unknown`: `cached::time` is backed by `web_time`,
so `#[cached]` / `TtlCache` work in the browser. This is a standalone workspace
crate (its Cargo.toml enables `cached` with `proc_macro,async,time_stores`),
not a `cargo run --example` target.

Build:
    cd examples/wasm && cargo build --target=wasm32-unknown-unknown
*/

use chrono::{DateTime, Utc};
use reqwasm::http::Request;
use yew::prelude::*;

use cached::TtlCache;
use cached::macros::cached;

const URL: &'static str = "https://echo.zuplo.io/";

#[derive(Clone)]
struct State {
    content: Option<String>,
    date: DateTime<Utc>,
}

#[function_component(App)]
fn app() -> Html {
    let state = use_state(|| State {
        content: None,
        date: Utc::now(),
    });
    let onclick = {
        let state = state.clone();
        let closure = move |_| {
            wasm_bindgen_futures::spawn_local(async move {
                let content = fetch("Body content".to_owned()).await;
                let date = Utc::now();
                state.set(State { content, date });
            })
        };
        Callback::once(closure)
    };
    html! {
        <>
            <button onclick = {onclick}>{"Fetch Content"}</button><br/>
            <span>{"Last clicked: "}{state.date}</span><br/>
            <div>
                {if let Some(response) = (*state).content.clone() {
                    response
                } else {
                    "Click the button".to_owned()
                }}
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
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()
}

fn main() {
    yew::start_app::<App>();
}
