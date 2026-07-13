//! [`FileSystem`] implementation backed by the real filesystem.

use crate::ports::{FileSystem, FsError};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Reads the vault from disk via `std::fs` / `walkdir`.
pub struct RealFileSystem;

impl FileSystem for RealFileSystem {
    fn list_org_files(&self, dir: &Path) -> Result<Vec<PathBuf>, FsError> {
        if !dir.is_dir() {
            return Err(FsError::NotADirectory(dir.to_path_buf()));
        }
        let mut paths = Vec::new();
        for entry in WalkDir::new(dir) {
            let entry = entry.map_err(|e| FsError::Io {
                path: e
                    .path()
                    .map_or_else(|| dir.to_path_buf(), Path::to_path_buf),
                message: e.to_string(),
            })?;
            if entry.file_type().is_file()
                && entry.path().extension().is_some_and(|ext| ext == "org")
            {
                paths.push(entry.path().to_path_buf());
            }
        }
        Ok(paths)
    }

    fn read_to_string(&self, path: &Path) -> Result<String, FsError> {
        std::fs::read_to_string(path).map_err(|e| FsError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    }

    fn file_exists(&self, path: &Path) -> bool {
        path.is_file()
    }
}
