# envoy-proto

Rust protobuf and gRPC bindings for the [Envoy data-plane-api](https://github.com/envoyproxy/envoy/tree/main/api).

This is the rust analogue of [`go-control-plane`](https://github.com/envoyproxy/go-control-plane)'s proto packages: generated code only, no xDS server.

## Usage

```toml
[dependencies]
envoy-proto = "0.1"
```

```rust
use envoy_proto::envoy::service::ext_proc::v3::{ProcessingRequest, ProcessingResponse};
```

## Versioning

The crate version (in `VERSION` / `Cargo.toml`) tracks bindings releases, not envoy releases. The envoy version that the protos were generated from is recorded in `ENVOY_VERSION` and published in each GitHub release's body.

## Regenerating

```shell
cargo xtask regen
```

This fetches envoy at the version in `ENVOY_VERSION` plus its proto dependencies, runs `tonic-build`, and writes the result to `crates/envoy-proto/src/generated/`. Commit the diff.

## Releasing

1. Drop the `-dev` suffix from `VERSION` and the version in `crates/envoy-proto/Cargo.toml`.
2. Open a PR, get it merged.
3. The `release` workflow tags, creates a GitHub release, and publishes to crates.io.
4. Open a follow-up PR bumping the version (e.g. `0.1.1-dev`).

## License

Apache-2.0. The generated code is derived from envoy data-plane-api and its proto dependencies (cncf/xds, protoc-gen-validate, googleapis), all Apache-2.0. See `NOTICE`.
