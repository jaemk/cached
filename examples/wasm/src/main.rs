use chrono::{DateTime, Utc};
use reqwasm::http::Request;
use yew::prelude::*;

use cached::proc_macro::cached;
use cached::TimedCache;

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
            <spam>{"Last clicked: "}{state.date}</spam><br/>
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
    type = "TimedCache<String, Option<String>>",
    create = "{ TimedCache::with_lifespan(5) }"
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
