use anyhow::{anyhow, Result};
use directories::ProjectDirs;
use std::path::PathBuf;

fn project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("", "", "inr")
        .ok_or_else(|| anyhow!("could not determine a home directory for this platform"))
}

pub fn data_dir() -> Result<PathBuf> {
    let dirs = project_dirs()?;
    let dir = dirs.data_dir().to_path_buf();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn db_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("inr.db"))
}
