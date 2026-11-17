#![forbid(unsafe_code)]

//! Repository tasks. `cargo xtask gen` regenerates `crates/asana-openapi/src/generated/` from
//! `spec/asana_oas.yaml` and the overlays; `cargo xtask gen --check` fails when the checked-in
//! code differs from what the specification and this generator produce.
//!
//! The generator has two halves that meet at [`naming`]:
//!
//! - [`models`] turns the specification's schemas into one struct per resource family, the typed
//!   GID list, and the shared serde helpers.
//! - [`endpoints`] turns its operations into endpoint descriptors, the `opt_fields` enums and the
//!   OAuth scope table.
//!
//! Each returns [`emit::GeneratedFile`]s; [`emit`] formats them and writes or compares them.

mod emit;
mod endpoints;
mod models;
mod naming;
mod spec;

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["gen"] => run(emit::Mode::Write, Half::Both),
        ["gen", "--check"] => run(emit::Mode::Check, Half::Both),
        ["gen", "--only", "models"] => run(emit::Mode::Write, Half::Models),
        ["gen", "--only", "endpoints"] => run(emit::Mode::Write, Half::Endpoints),
        _ => {
            eprintln!("usage: cargo xtask gen [--check | --only models | --only endpoints]");
            ExitCode::from(2)
        }
    }
}

/// Which half of the generator runs. `--only` exists so each half can be worked on while the
/// other is unfinished; it rewrites that half's files and leaves the rest of the directory alone.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Half {
    Both,
    Models,
    Endpoints,
}

/// The paths under `generated/` the models half owns; everything else is the endpoints half's.
const MODELS_PATHS: &[&str] = &["mod.rs", "ids.rs", "models.rs", "models"];
/// The paths under `generated/` the endpoints half owns.
const ENDPOINTS_PATHS: &[&str] = &[
    "endpoints.rs",
    "endpoints",
    "fields.rs",
    "fields",
    "scopes.rs",
];

fn run(mode: emit::Mode, half: Half) -> ExitCode {
    let root = repository_root();
    let result = spec::load(&root).and_then(|spec| {
        let mut files = Vec::new();
        let mut owned: Vec<&str> = Vec::new();
        if half != Half::Endpoints {
            files.extend(models::generate(&spec)?);
            owned.extend(MODELS_PATHS);
        }
        if half != Half::Models {
            files.extend(endpoints::generate(&spec)?);
            owned.extend(ENDPOINTS_PATHS);
        }
        emit::finish(
            &root.join("crates/asana-openapi/src/generated"),
            files,
            mode,
            &owned,
        )
    });
    match result {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("xtask gen: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The repository root: the parent of this crate's directory.
fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The generator's one error type: a sentence saying what could not be done and why.
pub type GenResult<T> = Result<T, String>;
