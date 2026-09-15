//! Command-line entry point for `clippy-shim`.

use std::process::ExitCode;

fn main() -> ExitCode {
    clippy_shim::run(std::env::args_os())
}
