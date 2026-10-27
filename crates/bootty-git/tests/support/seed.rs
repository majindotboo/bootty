use std::{fs, io, path::Path};

/// Copies immutable Git objects and refs; each case owns its working tree and mutable index.
pub fn repository(path: &Path, fixture: &str, bare: bool) -> io::Result<()> {
    super::repository_support::init(path, bare)?;
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| io::Error::other("workspace crate directory"))?
        .join("bootty-git/tests/fixtures")
        .join(fixture);
    let git = if bare {
        path.to_owned()
    } else {
        path.join(".git")
    };
    copy_tree(&fixture.join("git"), &git)?;
    if !bare {
        copy_tree(&fixture.join("worktree"), path)?;
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
