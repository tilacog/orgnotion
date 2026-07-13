//! Scans an org-roam vault directory (through the [`FileSystem`] port)
//! and parses every `.org` file into a [`Node`].

use crate::org_parser::{self, Node};
use crate::ports::{FileSystem, FsError};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Marker file that gives a directory an index node. Plain text, two
/// significant lines in any order: the file name of an `.org` file in
/// the same directory, and a `flat = true|false` option. The named
/// node's content is rendered on the directory's page, which takes the
/// node's title instead of the directory basename. With `flat = true`
/// the directory's remaining nodes are concatenated onto that same page
/// after the index node; with `flat = false` they keep their own child
/// pages.
pub const INDEX_MARKER: &str = ".INDEX";

/// A parsed [`INDEX_MARKER`] file.
#[derive(Debug)]
pub struct DirIndex {
    /// Path of the index node's `.org` file, as scanned (i.e. including
    /// the vault directory prefix, matching [`Node::file_path`]).
    pub file: PathBuf,
    /// Whether the directory's other nodes are merged onto the directory
    /// page after the index node, instead of getting their own pages.
    pub flat: bool,
}

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

    /// A [`INDEX_MARKER`] file is malformed or names a missing node.
    #[error("invalid {INDEX_MARKER} file in {dir:?}: {reason}")]
    InvalidIndex {
        /// Directory carrying the marker, relative to the vault root.
        dir: String,
        /// What is wrong with it.
        reason: String,
    },
}

/// The whole vault: every parsed node, in deterministic (path-sorted)
/// order.
#[derive(Debug)]
pub struct Vault {
    /// All parsed nodes.
    pub nodes: Vec<Node>,
    /// Directories carrying an [`INDEX_MARKER`] file, relative to the
    /// vault root (an empty path means the vault root itself). Only
    /// directories directly containing at least one node are recorded.
    pub indexes: BTreeMap<PathBuf, DirIndex>,
}

impl Vault {
    /// Build an ID → node lookup for resolving `[[id:...]]` links.
    #[must_use]
    pub fn id_index(&self) -> HashMap<&str, &Node> {
        self.nodes.iter().map(|n| (n.id.as_str(), n)).collect()
    }

    /// The index node of `dir` (relative to the vault root), if the
    /// directory carries an [`INDEX_MARKER`] file. Scanning guarantees
    /// the named file parsed as a node, so this resolves whenever the
    /// directory has an index.
    #[must_use]
    pub fn index_node(&self, dir: &Path) -> Option<&Node> {
        let index = self.indexes.get(dir)?;
        self.nodes.iter().find(|n| n.file_path == index.file)
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

    let indexes = dir_indexes(fs, vault_dir, &paths)?;
    Ok(Vault { nodes, indexes })
}

/// Parse the [`INDEX_MARKER`] file of every directory that directly
/// contains a node, keyed by directory relative to the vault root.
fn dir_indexes(
    fs: &impl FileSystem,
    vault_dir: &Path,
    paths: &[PathBuf],
) -> Result<BTreeMap<PathBuf, DirIndex>, VaultError> {
    let node_paths: BTreeSet<&Path> = paths.iter().map(PathBuf::as_path).collect();
    let dirs: BTreeSet<&Path> = paths.iter().filter_map(|path| path.parent()).collect();

    let mut indexes = BTreeMap::new();
    for dir in dirs {
        let marker = dir.join(INDEX_MARKER);
        if !fs.file_exists(&marker) {
            continue;
        }
        let rel_dir = dir.strip_prefix(vault_dir).unwrap_or(dir).to_path_buf();
        let invalid = |reason: String| VaultError::InvalidIndex {
            dir: rel_dir.display().to_string(),
            reason,
        };
        let text = fs.read_to_string(&marker)?;
        let (file_name, flat) = parse_index(&text).map_err(&invalid)?;
        let file = dir.join(file_name);
        if file.parent() != Some(dir) || !node_paths.contains(file.as_path()) {
            return Err(invalid(format!(
                "{file_name:?} is not an .org node file in this directory"
            )));
        }
        indexes.insert(rel_dir, DirIndex { file, flat });
    }
    Ok(indexes)
}

/// Parse an [`INDEX_MARKER`] file's text into its file name and `flat`
/// option. Blank lines are ignored; a line with `=` is an option, any
/// other line is the file name. Both are required, neither may repeat.
fn parse_index(text: &str) -> Result<(&str, bool), String> {
    let mut file = None;
    let mut flat = None;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() != "flat" {
                return Err(format!("unknown option {:?}", key.trim()));
            }
            if flat.is_some() {
                return Err("the \"flat\" option is given more than once".to_string());
            }
            flat = Some(match value.trim() {
                "true" => true,
                "false" => false,
                other => return Err(format!("\"flat\" must be true or false, got {other:?}")),
            });
        } else {
            if file.is_some() {
                return Err("more than one file path is given".to_string());
            }
            file = Some(line);
        }
    }
    match (file, flat) {
        (Some(file), Some(flat)) => Ok((file, flat)),
        (None, _) => Err("missing the index node's file path".to_string()),
        (_, None) => Err("missing the \"flat = true|false\" option".to_string()),
    }
}
