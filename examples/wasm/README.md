# WASM example

This example features a [yew](https://yew.rs/) application that fetches content
from an external API. The response body is cached using `cached` using 
`TimedCache` for 5 seconds

# Run this example

```shell
# Install `trunk`
cargo install trunk
# Add the required WASM target
rustup target add wasm32-unknown-unknown
# Start the server
trunk serve --open
```