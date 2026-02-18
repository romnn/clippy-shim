//! Small helper binary used by `cargo lint` and `cargo fc lint` to invoke
//! `cargo clippy` with repo-specific defaults.
//!
//! This crate exists because we want a single command that:
//!
//! - behaves well when invoked directly via `cargo lint ...` (a Cargo alias)
//! - behaves well when invoked repeatedly by `cargo-feature-combinations` / `cargo fc`
//!   across feature matrices
//!
//! A key subtlety is that `cargo fc` uses `-p/--package` to select *which packages*
//! to iterate, but then runs cargo in each selected package's directory and does not
//! forward `-p` to the underlying cargo invocation. This wrapper therefore must not
//! require `-p`/`--manifest-path` to implement its "lint only this crate" semantics.

use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Debug, Clone, Copy)]
struct ScopeFlags {
    has_package: bool,
    has_workspace: bool,
    has_manifest_path: bool,
}

#[derive(Debug, Clone, Copy)]
struct FeatureSelectionFlags {
    has_all_features: bool,
    has_features: bool,
    has_no_default_features: bool,
}

#[derive(Debug, Clone, Copy)]
struct DetectedFlags {
    scope: ScopeFlags,
    feature_selection: FeatureSelectionFlags,
    has_no_deps: bool,
    has_target_selection: bool,
}

/// Split CLI arguments into cargo arguments and clippy arguments.
///
/// Cargo accepts extra arguments for the underlying tool (here: rustc/Clippy)
/// after a `--` separator. We need to preserve that split so we can inject
/// cargo-level flags (e.g. `--no-deps`) without interfering with user-provided
/// clippy flags.
#[must_use]
fn split_args_on_double_dash(args: Vec<String>) -> (Vec<String>, Vec<String>) {
    let mut cargo_args = Vec::new();
    let mut clippy_args = Vec::new();

    let mut seen_double_dash = false;
    for arg in args {
        if !seen_double_dash && arg == "--" {
            seen_double_dash = true;
            continue;
        }

        if seen_double_dash {
            clippy_args.push(arg);
        } else {
            cargo_args.push(arg);
        }
    }
    (cargo_args, clippy_args)
}

/// Determine the workspace root directory.
///
/// We prefer `CARGO_WORKSPACE_DIR` when it is set (we set it via `.cargo/config.toml`)
/// because `env!("CARGO_MANIFEST_DIR")` points at this crate's directory.
#[must_use]
fn workspace_dir() -> PathBuf {
    let workspace_dir = std::env::var_os("CARGO_WORKSPACE_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);

    if let Some(workspace_dir) = workspace_dir {
        return workspace_dir;
    }

    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for _ in 0..2 {
        let Some(parent) = dir.parent() else {
            return dir;
        };
        dir = parent.to_path_buf();
    }

    dir
}

/// Convert a process exit code to the range supported by [`ExitCode`].
#[must_use]
fn exit_code_from_i32(code: i32) -> u8 {
    match u8::try_from(code) {
        Ok(code) => code,
        Err(_err) => {
            if code < 0 {
                0
            } else {
                255
            }
        }
    }
}

/// Convert a platform-specific exit status into an `ExitCode`.
#[must_use]
fn exit_code_from_status(status: std::process::ExitStatus) -> u8 {
    if let Some(code) = status.code() {
        return exit_code_from_i32(code);
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            let code = 128 + signal;
            return exit_code_from_i32(code);
        }
    }

    1
}

#[must_use]
fn detect_flags(user_cargo_args: &[String]) -> DetectedFlags {
    let mut flags = DetectedFlags {
        scope: ScopeFlags {
            has_package: false,
            has_workspace: false,
            has_manifest_path: false,
        },
        feature_selection: FeatureSelectionFlags {
            has_all_features: false,
            has_features: false,
            has_no_default_features: false,
        },
        has_no_deps: false,
        has_target_selection: false,
    };

    for arg in user_cargo_args {
        match arg.as_str() {
            "-p" | "--package" => {
                flags.scope.has_package = true;
            }
            "--manifest-path" => {
                flags.scope.has_manifest_path = true;
            }
            "--workspace" => {
                flags.scope.has_workspace = true;
            }
            "--no-deps" => {
                flags.has_no_deps = true;
            }
            "--all-targets" | "--lib" | "--bins" | "--tests" | "--benches" | "--examples"
            | "--targets" | "--bin" | "--test" | "--bench" | "--example" => {
                flags.has_target_selection = true;
            }
            "--all-features" => {
                flags.feature_selection.has_all_features = true;
            }
            "--features" => {
                flags.feature_selection.has_features = true;
            }
            "--no-default-features" => {
                flags.feature_selection.has_no_default_features = true;
            }
            _ => {
                if arg.starts_with("--package=") {
                    flags.scope.has_package = true;
                }
                if arg.starts_with("-p") && arg.len() > 2 {
                    flags.scope.has_package = true;
                }
                if arg.starts_with("--manifest-path=") {
                    flags.scope.has_manifest_path = true;
                }
                if arg.starts_with("--bin=")
                    || arg.starts_with("--test=")
                    || arg.starts_with("--bench=")
                    || arg.starts_with("--example=")
                {
                    flags.has_target_selection = true;
                }
                if arg.starts_with("--features=") {
                    flags.feature_selection.has_features = true;
                }
            }
        }
    }

    flags
}

#[must_use]
fn strip_workspace_if_contradictory(
    user_cargo_args: Vec<String>,
    flags: DetectedFlags,
) -> Vec<String> {
    if flags.scope.has_package || flags.scope.has_manifest_path {
        return user_cargo_args
            .into_iter()
            .filter(|arg| arg != "--workspace")
            .collect::<Vec<_>>();
    }

    user_cargo_args
}

#[must_use]
fn build_cargo_clippy_args(
    cargo_args: Vec<String>,
    user_cargo_args: Vec<String>,
    flags: DetectedFlags,
    is_workspace_root: bool,
) -> Vec<String> {
    let mut cargo_clippy_args = Vec::new();
    cargo_clippy_args.extend(cargo_args);

    // Only default to workspace linting when:
    // - we are invoked from the workspace root, and
    // - the user did not pass any narrower scope.
    //
    // This is critical for `cargo fc`, which runs this wrapper from each package's
    // directory without forwarding `-p`.
    if is_workspace_root
        && !flags.scope.has_package
        && !flags.scope.has_manifest_path
        && !flags.scope.has_workspace
    {
        cargo_clippy_args.push("--workspace".to_string());
    }

    // By default lint all targets. However, respect explicit target selection.
    if !flags.has_target_selection {
        cargo_clippy_args.push("--all-targets".to_string());
    }

    // Always prefer `--no-deps` so we don't fail on lints from dependency crates
    // when we enforce `-Dclippy::...`.
    if !flags.has_no_deps {
        cargo_clippy_args.push("--no-deps".to_string());
    }

    // Default to linting with all features enabled unless the user explicitly
    // selected some other feature mode.
    if !flags.feature_selection.has_all_features
        && !flags.feature_selection.has_features
        && !flags.feature_selection.has_no_default_features
    {
        cargo_clippy_args.push("--all-features".to_string());
    }

    cargo_clippy_args.extend(user_cargo_args);
    cargo_clippy_args
}

/// Invoke `cargo clippy` with repo defaults.
///
/// This is the core of the wrapper.
///
/// The wrapper is used in two important modes:
///
/// - **Direct invocation** via the `cargo lint` alias (see `.cargo/config.toml`).
///   Here the current working directory is often the workspace root and users
///   typically expect to lint the entire workspace unless they pass `-p`.
/// - **Feature-matrix invocation** via `cargo fc` (`cargo-feature-combinations`).
///   In that case the tool:
///
///   - consumes `-p/--package` itself to choose which packages to iterate
///   - runs the underlying cargo command with `current_dir` set to the selected
///     package directory
///   - does not forward `-p/--package` to the underlying cargo invocation
///
///   So this wrapper must work correctly even when it is called without `-p`.
///
/// ## Defaults and rationale
///
/// - **`--no-deps`**: always enabled (unless the user passed it) so that dependency
///   crates do not produce clippy diagnostics. We still compile dependencies, but we
///   avoid turning dependency lints into hard errors when we enforce `-D clippy::...`.
///
/// - **`--all-targets`**: enabled by default so we lint library, binaries, tests,
///   benches, and examples. If the user *already selected specific targets*
///   (`--lib`, `--bin`, `--tests`, etc.), we do not add `--all-targets`.
///
/// - **`--all-features`**: enabled by default unless the user already provided an
///   explicit feature selection (`--all-features`, `--features`, or
///   `--no-default-features`). This keeps `cargo lint` useful without requiring
///   explicit feature flags.
///
/// - **`--workspace`**: only enabled by default when running from the workspace root
///   and the user did not specify a narrower scope (`-p`, `--manifest-path`, or
///   `--workspace`). When running inside a package directory (as `cargo fc` does),
///   we *do not* force `--workspace`.
///
/// - **`-Dclippy::all` / `-Dclippy::pedantic`**: always appended to enforce a strict
///   lint baseline for this repository. These are intentionally appended after any
///   user-provided clippy args so the wrapper remains authoritative.
///
/// # Errors
///
/// Returns an error if spawning or waiting on the `cargo clippy` process fails.
fn run_cargo_clippy(
    cargo_args: Vec<String>,
    args: Vec<String>,
) -> Result<std::process::ExitStatus, std::io::Error> {
    let (user_cargo_args, user_clippy_args) = split_args_on_double_dash(args);

    let workspace_dir = workspace_dir();
    let is_workspace_root = std::env::current_dir()
        .ok()
        .is_some_and(|current_dir| current_dir == workspace_dir);

    let flags = detect_flags(&user_cargo_args);

    // If the user explicitly scoped to a single package or manifest path, we treat
    // `--workspace` as a contradiction and drop it.
    let user_cargo_args = strip_workspace_if_contradictory(user_cargo_args, flags);

    let cargo_clippy_args =
        build_cargo_clippy_args(cargo_args, user_cargo_args, flags, is_workspace_root);

    let mut command = std::process::Command::new("cargo");
    command.arg("clippy");
    command.args(cargo_clippy_args);
    command.arg("--");
    command.args(user_clippy_args);
    command.arg("-Dclippy::all");
    command.arg("-Dclippy::pedantic");

    command.status()
}

fn usage(program_name: &str) {
    eprintln!("Usage:");
    eprintln!("  {program_name} lint [cargo clippy args] [-- clippy args]");
    eprintln!("  {program_name} fixit [cargo clippy args] [-- clippy args]");
}

fn main() -> ExitCode {
    let mut args_iter = std::env::args();
    let program_name = args_iter
        .next()
        .unwrap_or_else(|| "clippy-wrapper".to_string());

    let Some(subcommand) = args_iter.next() else {
        usage(&program_name);
        return ExitCode::from(2);
    };

    let remaining_args: Vec<String> = args_iter.collect();

    let (cargo_args, args) = match subcommand.as_str() {
        "lint" => (Vec::new(), remaining_args),
        "fixit" => (
            vec![
                "--fix".to_string(),
                "--allow-dirty".to_string(),
                "--allow-staged".to_string(),
            ],
            remaining_args,
        ),
        "-h" | "--help" | "help" => {
            usage(&program_name);
            return ExitCode::from(0);
        }
        _ => {
            eprintln!("unknown subcommand: {subcommand}");
            usage(&program_name);
            return ExitCode::from(2);
        }
    };

    let status = match run_cargo_clippy(cargo_args, args) {
        Ok(status) => status,
        Err(err) => {
            eprintln!("failed to run cargo clippy: {err}");
            return ExitCode::from(1);
        }
    };

    if status.success() {
        return ExitCode::SUCCESS;
    }

    ExitCode::from(exit_code_from_status(status))
}
