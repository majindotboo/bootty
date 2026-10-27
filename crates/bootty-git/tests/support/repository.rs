//! Minimal empty repositories; behavior still runs through real Git.
use std::{fs, io, path::Path};

pub fn init(path: &Path, bare: bool) -> io::Result<()> {
    let directory = if bare {
        path.to_owned()
    } else {
        path.join(".git")
    };
    fs::create_dir_all(directory.join("objects"))?;
    fs::create_dir_all(directory.join("info"))?;
    fs::create_dir_all(directory.join("refs/heads"))?;
    fs::create_dir_all(directory.join("refs/tags"))?;
    fs::write(directory.join("HEAD"), "ref: refs/heads/main\n")?;
    fs::write(
        directory.join("config"),
        format!(
            "[core]\nrepositoryformatversion = 0\nbare = {bare}\nhooksPath = /dev/null\n[user]\nname = Bootty Test\nemail = test@example.invalid\n[commit]\ngpgsign = false\n[init]\ndefaultBranch = main\n"
        ),
    )
}
