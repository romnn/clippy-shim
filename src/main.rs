use std::process::ExitCode;

fn main() -> ExitCode {
    clippy_shim::run(std::env::args_os())
}
