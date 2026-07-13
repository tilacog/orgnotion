//! The I/O boundaries of the application, expressed as traits.
//!
//! All business logic (vault scanning, validation, conversion, run
//! orchestration) depends only on these traits. Concrete implementations
//! that touch the real world — the filesystem, the Notion REST API, the
//! system clock, process environment variables — live in
//! [`crate::adapters`] and are injected at the edge (in `main`).
//! Tests inject in-memory fakes instead, so the whole run flow is
//! unit-testable without a network or a real vault on disk.

use notionrs_types::object::block::Block;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Read access to the filesystem holding the Org-roam vault.
pub trait FileSystem {
    /// List every `.org` file under `dir`, recursively.
    ///
    /// Ordering is not guaranteed; callers sort for determinism.
    ///
    /// # Errors
    ///
    /// Returns an error if `dir` does not exist, is not a directory, or
    /// cannot be traversed.
    fn list_org_files(&self, dir: &Path) -> Result<Vec<PathBuf>, FsError>;

    /// Read the full contents of one file as UTF-8 text.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or is not valid UTF-8.
    fn read_to_string(&self, path: &Path) -> Result<String, FsError>;

    /// Whether `path` exists and is a regular file (used to detect marker
    /// files). Lookup failures collapse to `false`.
    fn file_exists(&self, path: &Path) -> bool;
}

/// Filesystem failure, decoupled from `std::io` so fakes can construct it.
#[derive(Debug, thiserror::Error)]
pub enum FsError {
    /// The vault path is missing or is not a directory.
    #[error("vault directory not found or not a directory: {}", .0.display())]
    NotADirectory(PathBuf),

    /// A file or directory could not be read.
    #[error("failed to read {}: {message}", .path.display())]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Human-readable cause.
        message: String,
    },
}

/// A page created via the Notion API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedPage {
    /// The Notion page ID.
    pub id: String,
    /// The user-facing `notion.so` URL of the page.
    pub url: String,
}

/// One block read back from Notion: the typed block content plus the
/// listing metadata needed to recurse into nested children. Deliberately
/// thinner than the API's full block response (timestamps, editors, …),
/// which nothing here needs and fakes would otherwise have to fabricate.
#[derive(Debug, Clone)]
pub struct ChildBlock {
    /// The Notion block ID.
    pub id: String,
    /// Whether the block has nested children of its own.
    pub has_children: bool,
    /// The typed block content.
    pub block: Block,
}

/// One page of results from listing a block's children.
#[derive(Debug, Clone)]
pub struct ChildrenPage {
    /// The block objects in this page of results.
    pub blocks: Vec<ChildBlock>,
    /// Cursor to pass back to fetch the next page, if any.
    pub next_cursor: Option<String>,
}

/// The subset of the Notion REST API this tool needs.
///
/// Pagination is exposed cursor-by-cursor (rather than hidden inside the
/// implementation) so the cursor-following logic itself is plain code,
/// unit-testable against a fake.
///
/// Methods are async; independent calls are issued with bounded
/// concurrency by the run orchestration. Everything runs buffered on one
/// task (never spawned), so implementations' futures need not be `Send`
/// — which is exactly why plain `async fn` (no `Send` bound) is the
/// right signature here.
#[allow(async_fn_in_trait)]
pub trait NotionApi {
    /// `POST /v1/pages`: create a page with a plain-text title as a child
    /// of `parent_page_id`. Content is appended separately.
    ///
    /// # Errors
    ///
    /// Returns an error on transport failure or a non-success API status
    /// (after the implementation's retry policy is exhausted).
    async fn create_page(
        &self,
        parent_page_id: &str,
        title: &str,
    ) -> Result<CreatedPage, NotionError>;

    /// `PATCH /v1/blocks/{block_id}/children`: append child blocks.
    ///
    /// Callers must respect Notion's limit of 100 children per call;
    /// chunking is orchestration logic, not transport logic.
    ///
    /// # Errors
    ///
    /// Returns an error on transport failure or a non-success API status.
    async fn append_children(&self, block_id: &str, children: &[Block]) -> Result<(), NotionError>;

    /// `GET /v1/blocks/{block_id}/children`: fetch one page of children,
    /// starting at `cursor` (or the beginning when `None`).
    ///
    /// # Errors
    ///
    /// Returns an error on transport failure or a non-success API status.
    async fn list_children(
        &self,
        block_id: &str,
        cursor: Option<&str>,
    ) -> Result<ChildrenPage, NotionError>;
}

/// Notion API failure.
#[derive(Debug, thiserror::Error)]
pub enum NotionError {
    /// The request never produced an HTTP response (DNS, TLS, timeouts…).
    #[error("Notion API request failed: {0}")]
    Transport(String),

    /// The API returned a non-success status (retries exhausted for
    /// retryable statuses).
    #[error(
        "Notion API returned {status}: {body}{hint}",
        hint = auth_hint(*.status)
    )]
    Api {
        /// HTTP status code.
        status: u16,
        /// Response body, for diagnostics.
        body: String,
    },

    /// The API responded with JSON we could not interpret.
    #[error("unexpected response shape from Notion API: {0}")]
    UnexpectedShape(String),
}

fn auth_hint(status: u16) -> &'static str {
    if status == 401 || status == 403 {
        "\nCheck that NOTION_TOKEN is a valid integration token and that the \
         parent page has been shared with the integration."
    } else {
        ""
    }
}

/// Time source, so default snapshot titles and elapsed-time reporting are
/// deterministic in tests.
pub trait Clock {
    /// Current wall-clock time as an ISO-8601 / RFC 3339 UTC string,
    /// e.g. `2026-07-12T14:03:00Z`.
    fn now_iso8601(&self) -> String;

    /// Monotonic time since some fixed origin (used for elapsed-time
    /// reporting; only differences are meaningful).
    fn monotonic(&self) -> Duration;
}

/// Process environment access.
pub trait Env {
    /// Look up an environment variable, `None` if unset or not UTF-8.
    fn var(&self, key: &str) -> Option<String>;
}

/// Progress output sink; the CLI prints to stdout, tests capture lines.
pub trait Reporter {
    /// Emit one line of progress information.
    fn info(&mut self, message: &str);
}
