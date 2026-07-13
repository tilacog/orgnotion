//! In-memory fake implementations of the I/O ports, shared by the test
//! suite. No network or real filesystem involved.

// Each integration-test binary compiles this module independently and
// uses a different subset of it.
#![allow(dead_code)]

use notionrs_types::object::block::Block;
use orgnotion::ports::{
    ChildBlock, ChildrenPage, Clock, CreatedPage, Env, FileSystem, FsError, NotionApi, NotionError,
    Reporter,
};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A vault held entirely in memory: path → file contents.
#[derive(Default)]
pub struct InMemoryFileSystem {
    pub files: BTreeMap<PathBuf, String>,
}

impl InMemoryFileSystem {
    pub fn with_files(files: &[(&str, &str)]) -> Self {
        Self {
            files: files
                .iter()
                .map(|(p, c)| (PathBuf::from(p), (*c).to_string()))
                .collect(),
        }
    }
}

impl FileSystem for InMemoryFileSystem {
    fn list_org_files(&self, dir: &Path) -> Result<Vec<PathBuf>, FsError> {
        if self.files.is_empty() {
            return Err(FsError::NotADirectory(dir.to_path_buf()));
        }
        Ok(self
            .files
            .keys()
            .filter(|p| p.starts_with(dir) && p.extension().is_some_and(|e| e == "org"))
            .cloned()
            .collect())
    }

    fn read_to_string(&self, path: &Path) -> Result<String, FsError> {
        self.files.get(path).cloned().ok_or_else(|| FsError::Io {
            path: path.to_path_buf(),
            message: "no such file".to_string(),
        })
    }

    fn file_exists(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }
}

/// One recorded page creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRecord {
    pub id: String,
    pub parent_id: String,
    pub title: String,
}

/// In-memory Notion: records created pages and appended children, and
/// serves them back (paginated) for post-validation reads.
#[derive(Default)]
pub struct FakeNotion {
    state: RefCell<FakeNotionState>,
    /// Page size used when listing children, to exercise pagination.
    pub list_page_size: usize,
    /// When set, `create_page` fails once the fake has created this many
    /// pages (simulates a mid-run API failure).
    pub fail_create_after: Option<usize>,
    /// When set, `append_children` always fails with this status.
    pub fail_append_status: Option<u16>,
    /// When true, `list_children` returns empty results regardless of what
    /// was appended (simulates content silently not landing in Notion).
    pub serve_empty_children: bool,
    /// When true, the second `create_page` call (the first page after the
    /// snapshot root) yields to the executor several times before
    /// recording. Under concurrent creation, later requests would be
    /// recorded first — letting tests pin down that page creation stays
    /// sequential (Notion shows siblings in creation order).
    pub stagger_creates: bool,
}

#[derive(Default)]
struct FakeNotionState {
    pages: Vec<PageRecord>,
    children: HashMap<String, Vec<Block>>,
    append_calls: Vec<(String, usize)>,
    list_calls: Vec<(String, Option<String>)>,
    creates_started: usize,
}

impl FakeNotion {
    pub fn new() -> Self {
        Self {
            list_page_size: 100,
            ..Self::default()
        }
    }

    pub fn pages(&self) -> Vec<PageRecord> {
        self.state.borrow().pages.clone()
    }

    pub fn children_of(&self, block_id: &str) -> Vec<Block> {
        self.state
            .borrow()
            .children
            .get(block_id)
            .cloned()
            .unwrap_or_default()
    }

    /// The ID the fake assigns to the `index`-th child appended under
    /// `parent_id` — lets tests append nested children under a block.
    pub fn child_id(parent_id: &str, index: usize) -> String {
        format!("{parent_id}-child-{index}")
    }

    /// Every `append_children` call as `(block_id, chunk_len)`.
    pub fn append_calls(&self) -> Vec<(String, usize)> {
        self.state.borrow().append_calls.clone()
    }

    /// Every `list_children` call as `(block_id, cursor)`.
    pub fn list_calls(&self) -> Vec<(String, Option<String>)> {
        self.state.borrow().list_calls.clone()
    }

    fn page_url(id: &str) -> String {
        format!("https://www.notion.so/{id}")
    }
}

// The fake's async methods complete synchronously (no awaits), so the
// interior RefCell borrows never span an await point even under buffered
// concurrency.
impl NotionApi for FakeNotion {
    async fn create_page(
        &self,
        parent_page_id: &str,
        title: &str,
    ) -> Result<CreatedPage, NotionError> {
        let started = {
            let mut state = self.state.borrow_mut();
            state.creates_started += 1;
            state.creates_started - 1
        };
        if self.stagger_creates && started == 1 {
            for _ in 0..4 {
                tokio::task::yield_now().await;
            }
        }
        let mut state = self.state.borrow_mut();
        if let Some(limit) = self.fail_create_after
            && state.pages.len() >= limit
        {
            return Err(NotionError::Api {
                status: 500,
                body: "fake: create_page failure injected".to_string(),
            });
        }
        let id = format!("page-{}", state.pages.len());
        state.pages.push(PageRecord {
            id: id.clone(),
            parent_id: parent_page_id.to_string(),
            title: title.to_string(),
        });
        Ok(CreatedPage {
            url: Self::page_url(&id),
            id,
        })
    }

    async fn append_children(&self, block_id: &str, children: &[Block]) -> Result<(), NotionError> {
        if let Some(status) = self.fail_append_status {
            return Err(NotionError::Api {
                status,
                body: "fake: append failure injected".to_string(),
            });
        }
        let mut state = self.state.borrow_mut();
        state
            .append_calls
            .push((block_id.to_string(), children.len()));
        state
            .children
            .entry(block_id.to_string())
            .or_default()
            .extend(children.iter().cloned());
        Ok(())
    }

    async fn list_children(
        &self,
        block_id: &str,
        cursor: Option<&str>,
    ) -> Result<ChildrenPage, NotionError> {
        let mut state = self.state.borrow_mut();
        state
            .list_calls
            .push((block_id.to_string(), cursor.map(ToString::to_string)));

        if self.serve_empty_children {
            return Ok(ChildrenPage {
                blocks: vec![],
                next_cursor: None,
            });
        }

        let all = state.children.get(block_id).cloned().unwrap_or_default();
        let start: usize = cursor.map_or(0, |c| c.parse().expect("fake cursors are offsets"));
        let end = (start + self.list_page_size).min(all.len());
        let next_cursor = (end < all.len()).then(|| end.to_string());
        // Child IDs are deterministic (`Self::child_id`); a child "has
        // children" iff something was appended under that derived ID.
        let blocks = all[start..end]
            .iter()
            .enumerate()
            .map(|(offset, block)| {
                let id = Self::child_id(block_id, start + offset);
                ChildBlock {
                    has_children: state.children.contains_key(&id),
                    id,
                    block: block.clone(),
                }
            })
            .collect();
        Ok(ChildrenPage {
            blocks,
            next_cursor,
        })
    }
}

/// A clock frozen at a fixed timestamp; monotonic time advances by one
/// second per call.
pub struct FixedClock {
    pub iso: String,
    ticks: RefCell<u64>,
}

impl FixedClock {
    pub fn at(iso: &str) -> Self {
        Self {
            iso: iso.to_string(),
            ticks: RefCell::new(0),
        }
    }
}

impl Clock for FixedClock {
    fn now_iso8601(&self) -> String {
        self.iso.clone()
    }

    fn monotonic(&self) -> Duration {
        let mut ticks = self.ticks.borrow_mut();
        *ticks += 1;
        Duration::from_secs(*ticks)
    }
}

/// Environment variables from a fixed map.
#[derive(Default)]
pub struct FakeEnv {
    pub vars: HashMap<String, String>,
}

impl FakeEnv {
    pub fn with(vars: &[(&str, &str)]) -> Self {
        Self {
            vars: vars
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        }
    }
}

impl Env for FakeEnv {
    fn var(&self, key: &str) -> Option<String> {
        self.vars.get(key).cloned()
    }
}

/// Collects progress lines for assertions.
#[derive(Default)]
pub struct CollectingReporter {
    pub lines: Vec<String>,
}

impl Reporter for CollectingReporter {
    fn info(&mut self, message: &str) {
        self.lines.push(message.to_string());
    }
}
