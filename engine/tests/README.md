# Tests

All test cases and test helpers live in this directory.

- `unit/` mirrors the source modules for unit tests. Each source module loads
  its test file with `#[cfg(test)]` and `#[path = "..."]`, so tests can still
  access private functions and types without changing the public API.
- `parallel_index.rs` and `retrieval.rs` contain integration tests that exercise
  the public API and command-line programs.

Run the default suite with `cargo test --locked --offline`.
Run CPU-only tests with `cargo test --locked --offline --no-default-features`.
Hardware GPU tests are ignored by default; run them with
`cargo test --locked --offline --features gpu-sort -- --ignored` on a Metal GPU.
