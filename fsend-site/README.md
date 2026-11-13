# fsend-site

This directory contains the webapp for fsend. Bun is used for development and bundling.

## Development

```bash
cd fsend-site
bun install
bun run dev      # http://localhost:3000
```

Building needs Rust with the `wasm32-unknown-unknown` target and the
`wasm-bindgen` CLI at the version `fsend-proto-wasm` pins:

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.130
```

`bun run build:wasm` builds the protocol code into `src/lib/proto/pkg`; `dev`,
`build` and `test` run it first. The TypeScript types in
`src/lib/proto/generated` come from the Rust types; regenerate them with
`UPDATE_BINDINGS=1 cargo test -p fsend-proto-wasm`.

## Building

```bash
cd fsend-site
bun install
bun run build
```
