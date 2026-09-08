pub mod benchmark;
pub mod build;
pub mod cancellation;
mod cli;
mod clock;
mod command;
pub mod daemon;
mod filesystem;
pub mod hakari;
pub mod install;
pub mod launch;
pub mod package;
pub mod pre_commit;
mod process;
pub mod release;
pub mod site;

fn development_names() -> bootty_identity::ApplicationNames {
    bootty_identity::development_names_for_workspace(&workspace_root())
}

fn workspace_root() -> std::path::PathBuf {
    let current = std::env::current_dir()
        .and_then(|path| path.canonicalize())
        .expect("xtasks need a current working directory");
    current
        .ancestors()
        .find(|candidate| candidate.join(".git").exists())
        .map_or(current.clone(), std::path::Path::to_path_buf)
}

pub use cli::run;
