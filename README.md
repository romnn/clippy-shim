# clippy-shim

A tiny wrapper around `cargo clippy` intended to be invoked via Cargo aliases.

### What it does

- Adds defaults for `cargo clippy`:
  - `--no-deps`
  - `--all-targets`
  - `--all-features`
  - `--workspace` only when invoked from the workspace root and no narrower scope was given
- Appends strict clippy lints:
  - `-Dclippy::all`
  - `-Dclippy::pedantic`

The behavior is designed to work well with `cargo-feature-combinations` (`cargo fc`),
which runs cargo from individual package directories and does not forward `-p`.

### Usage via `.cargo/config.toml`

Create a workspace member binary (e.g. `my-clippy-shim`) with a minimal `main`:

```rust
fn main() -> std::process::ExitCode {
    clippy_shim::run(std::env::args_os())
}
```

Add an alias pointing at a small binary that calls this library.

Example:

```toml
[alias]
lint = "run -p my-clippy-shim -- lint"
fixit = "run -p my-clippy-shim -- fixit"
```

Then you can use:

```bash
cargo lint
cargo lint -p my_crate
cargo fixit
```
