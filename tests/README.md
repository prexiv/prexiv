# PreXiv Test Notes

The website test surface is now Rust-first. The old Node/SQLite smoke tests were
removed with the legacy website runtime so the repository has one production
path: Rust + PostgreSQL.

## Local Checks

From the repository root:

```sh
npm run fmt:check
npm run test
npm run lint
npm run build
```

These map to the Rust commands under `rust/`:

```sh
cd rust
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

## MCP Bridge Check

The independent MCP package still lives under `mcp/`. Install its dependencies
there and run the syntax check:

```sh
cd mcp
npm ci
npm run check
```

`tests/prexiv_mcp_test.py` is an optional live smoke test for the MCP bridge. It
requires a running Rust PreXiv server and `mcp/` dependencies:

```sh
cd rust
export DATABASE_URL=postgres://prexiv:prexiv@127.0.0.1:5432/prexiv_dev
export PREXIV_DATA_KEY="$(openssl rand -hex 32)"
cargo run

# another terminal
BASE=http://localhost:3001/api/v1 python3 tests/prexiv_mcp_test.py
```
