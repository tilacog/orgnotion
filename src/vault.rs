//! Scans an org-roam vault directory (through the [`FileSystem`] port)
//! and parses every `.org` file into a [`Node`].

use crate::org_parser::{self, Node};
use crate::ports::{FileSystem, FsError};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Marker file that turns a directory "continuous": the `.org` files
/// directly inside it (non-recursive) are published as a single Notion
/// page, concatenated in file-name order.
pub const CONTINUOUS_MARKER: &str = ".CONTINUOUS";

/// Failure while building the vault index.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    /// The vault directory could not be listed or a file could not be read.
    #[error(transparent)]
    Fs(#[from] FsError),

    /// A `.org` file could not be parsed into a node.
    #[error(transparent)]
    Parse(#[from] org_parser::ParseError),

    /// Two files claim the same org-roam ID.
    #[error(
        "duplicate org-roam ID {id:?} found in both {first} and {second} — \
         IDs must be unique across the vault"
    )]
    DuplicateId {
        /// The duplicated ID.
        id: String,
        /// First file seen with the ID.
        first: String,
        /// Second file seen with the ID.
        second: String,
    },
}

/// The whole vault: every parsed node, in deterministic (path-sorted)
/// order.
#[derive(Debug)]
pub struct Vault {
    /// All parsed nodes.
    pub nodes: Vec<Node>,
    /// Directories carrying a [`CONTINUOUS_MARKER`] file, relative to the
    /// vault root (an empty path means the vault root itself). Only
    /// directories directly containing at least one node are recorded.
    pub continuous_dirs: BTreeSet<PathBuf>,
}

impl Vault {
    /// Build an ID → node lookup for resolving `[[id:...]]` links.
    #[must_use]
    pub fn id_index(&self) -> HashMap<&str, &Node> {
        self.nodes.iter().map(|n| (n.id.as_str(), n)).collect()
    }
}

/// Recursively scan `vault_dir` for `.org` files and parse each one.
///
/// Files are processed in sorted path order so snapshot runs are
/// reproducible.
///
/// # Errors
///
/// Fails on the first unreadable file, unparsable file, or duplicate ID —
/// a broken vault should never silently produce a partial snapshot.
pub fn scan(fs: &impl FileSystem, vault_dir: &Path) -> Result<Vault, VaultError> {
    let mut paths = fs.list_org_files(vault_dir)?;
    paths.sort();

    let mut nodes = Vec::new();
    let mut seen_ids: HashMap<String, String> = HashMap::new();

    for path in &paths {
        let text = fs.read_to_string(path)?;
        let node = org_parser::parse_node(path, &text)?;

        if let Some(existing) = seen_ids.get(&node.id) {
            return Err(VaultError::DuplicateId {
                id: node.id.clone(),
                first: existing.clone(),
                second: path.display().to_string(),
            });
        }
        seen_ids.insert(node.id.clone(), path.display().to_string());
        nodes.push(node);
    }

    Ok(Vault {
        nodes,
        continuous_dirs: continuous_dirs(fs, vault_dir, &paths),
    })
}

/// The relative directories among the org files' parents that carry a
/// [`CONTINUOUS_MARKER`] file.
fn continuous_dirs(fs: &impl FileSystem, vault_dir: &Path, paths: &[PathBuf]) -> BTreeSet<PathBuf> {
    paths
        .iter()
        .filter_map(|path| path.parent())
        .filter(|dir| fs.file_exists(&dir.join(CONTINUOUS_MARKER)))
        .map(|dir| dir.strip_prefix(vault_dir).unwrap_or(dir).to_path_buf())
        .collect()
}
