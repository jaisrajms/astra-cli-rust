# Astra CLI — repo guide

Thin gRPC client over the `astrad` daemon (the Rust engine in
`../astra-engine-rust`). The daemon is the backend; the CLI is an interface only.

## Naming / vendor-name hygiene (mandatory)
- Never write the reference CLI's product name (`opencode`) — or any external vendor/product name —
  in code, comments, docstrings, identifiers, log/error strings, or **git commit messages**. Refer to
  it as **"the reference CLI"** / **"the reference implementation"** instead. (Existing commits that
  predate this rule are left as-is; do not amend them.)
- When mirroring the reference CLI's interface/behavior, cite it only as "the reference CLI" and keep
  the port faithful without attributing it by product name in the source.

## Build / test
- `. "$HOME/.cargo/env"` first; `cargo build`, `cargo test`, `cargo clippy --all-targets`, `cargo fmt`.
- The CLI links `astra-proto` from `../astra-engine-rust/crates/astra-proto` (path dependency), so a
  proto change in the engine rebuilds the CLI's generated types.
