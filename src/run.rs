//! Top-level run orchestration: scan → parse → pre-validate → create root
//! page → create blank node pages → convert + write content →
//! post-validate → report.
//!
//! Generic over every I/O port, so the whole flow is unit-testable with
//! in-memory fakes.

use crate::converter::Converter;
use crate::notion::append_children_chunked;
use crate::org_parser::{Node, OrgBlock, Span};
use crate::ports::{Clock, Env, NotionApi, Reporter};
use crate::transformers::{anchors_to_bold, unreviewed_banner};
use crate::vault::VaultError;
use crate::{validate, vault};
use futures::stream::{self, StreamExt, TryStreamExt};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Environment variable holding the Notion integration token.
pub const TOKEN_ENV_VAR: &str = "NOTION_TOKEN";
/// Environment variable fallback for `--parent-page-id`.
pub const PARENT_PAGE_ID_ENV_VAR: &str = "ORGNOTION_PARENT_PAGE_ID";

/// Inputs to a run, as resolved from the CLI.
pub struct RunConfig {
    /// Path to the org-roam vault directory.
    pub vault_dir: PathBuf,
    /// `--parent-page-id`, if given (falls back to
    /// [`PARENT_PAGE_ID_ENV_VAR`]).
    pub parent_page_id: Option<String>,
    /// `--title`, if given (defaults to a timestamped title).
    pub title: Option<String>,
    /// `--dry-run`: stop after pre-validation, write nothing.
    pub dry_run: bool,
    /// Maximum number of Notion API calls in flight at once (≥ 1).
    /// Applies to content writes and post-validation reads; page creation
    /// is always sequential so sibling pages keep their sorted order
    /// (Notion shows children in creation order and cannot reorder them).
    pub concurrency: usize,
}

/// Why a run failed. Each variant maps to a distinct exit code in `main`.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The vault could not be scanned or parsed.
    #[error(transparent)]
    Vault(#[from] VaultError),

    /// Pre-validation found broken links; nothing was written to Notion.
    #[error("pre-validation failed: {} broken link(s); nothing was written to Notion", .0.len())]
    PreValidation(Vec<validate::BrokenLink>),

    /// No parent page ID from `--parent-page-id` or the environment.
    #[error(
        "no parent page ID: pass --parent-page-id or set the \
         {PARENT_PAGE_ID_ENV_VAR} environment variable"
    )]
    MissingParentPageId,

    /// `NOTION_TOKEN` is unset.
    #[error(
        "{TOKEN_ENV_VAR} environment variable is not set. Create a Notion \
         internal integration, copy its token, and export it as {TOKEN_ENV_VAR}."
    )]
    MissingToken,

    /// A Notion API call failed mid-run; the snapshot (if a root page URL
    /// is present) is incomplete and should be deleted manually.
    #[error("{context}: {source}")]
    Api {
        /// What the run was doing when the call failed.
        context: String,
        /// The underlying API failure.
        source: crate::ports::NotionError,
        /// Root page URL, if the root page had already been created.
        root_url: Option<String>,
    },

    /// Content was written but reading it back found missing/incorrect
    /// mentions; the snapshot should be deleted and the run retried.
    #[error("post-validation failed for {} node(s)", .failures.len())]
    PostValidation {
        /// URL of the (invalid) snapshot root page.
        root_url: String,
        /// One entry per node that failed, with its missing link targets.
        failures: Vec<validate::PostValidationResult>,
    },
}

/// Summary of a successful run.
#[derive(Debug)]
pub struct RunReport {
    /// Number of node pages created (0 for dry runs).
    pub node_count: usize,
    /// Number of content blocks written (0 for dry runs).
    pub block_count: usize,
    /// URL of the snapshot root page (`None` for dry runs).
    pub root_url: Option<String>,
}

/// Execute a full snapshot run.
///
/// # Errors
///
/// See [`RunError`]; any error means the run did not produce a fully
/// validated snapshot.
pub async fn execute(
    config: &RunConfig,
    fs: &impl crate::ports::FileSystem,
    notion: &impl NotionApi,
    clock: &impl Clock,
    env: &impl Env,
    reporter: &mut impl Reporter,
) -> Result<RunReport, RunError> {
    let started = clock.monotonic();

    reporter.info(&format!("Scanning vault: {}", config.vault_dir.display()));
    let vault = vault::scan(fs, &config.vault_dir)?;
    reporter.info(&format!("Found {} node(s).", vault.nodes.len()));

    reporter.info("Pre-validating internal links...");
    validate::pre_validate(&vault).map_err(RunError::PreValidation)?;
    reporter.info("All links resolve locally.");

    if config.dry_run {
        return Ok(report_dry_run(&vault, &config.vault_dir, reporter));
    }

    let parent_page_id = config
        .parent_page_id
        .clone()
        .or_else(|| env.var(PARENT_PAGE_ID_ENV_VAR))
        .ok_or(RunError::MissingParentPageId)?;

    let title = config
        .title
        .clone()
        .unwrap_or_else(|| format!("Org-roam snapshot {}", clock.now_iso8601()));

    let report = publish(
        &vault,
        &config.vault_dir,
        &parent_page_id,
        &title,
        config.concurrency.max(1),
        notion,
        reporter,
    )
    .await?;

    let elapsed = clock.monotonic().saturating_sub(started);
    reporter.info("");
    reporter.info(&format!(
        "Snapshot published and fully validated: {} node(s), {} block(s), {:.1}s elapsed.",
        report.node_count,
        report.block_count,
        elapsed.as_secs_f64()
    ));
    reporter.info(&format!(
        "Root page: {}",
        report.root_url.as_deref().unwrap_or_default()
    ));

    Ok(report)
}

fn report_dry_run(
    vault: &vault::Vault,
    vault_dir: &Path,
    reporter: &mut impl Reporter,
) -> RunReport {
    reporter.info("");
    reporter.info("Dry run: vault is valid and ready to publish. Planned structure:");
    // Nodes are in path-sorted order, so each directory's nodes are
    // contiguous and ancestors are first encountered shallowest-first.
    let mut printed_dirs: BTreeSet<PathBuf> = BTreeSet::new();
    for node in &vault.nodes {
        let dir = node_dir(node, vault_dir);
        for ancestor in ancestors_shallowest_first(&dir) {
            if printed_dirs.insert(ancestor.clone()) {
                let indent = "  ".repeat(ancestor.components().count());
                let continuous = if vault.continuous_dirs.contains(&ancestor) {
                    " (continuous)"
                } else {
                    ""
                };
                reporter.info(&format!(
                    "{indent}- {}/{continuous}",
                    ancestor.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
        }
        let indent = "  ".repeat(dir.components().count() + 1);
        reporter.info(&format!(
            "{indent}- {} ({}, {} block(s), {} link(s))",
            node.title,
            node.id,
            node.blocks.len(),
            node.links.len()
        ));
    }
    reporter.info("Nothing was written to Notion.");
    RunReport {
        node_count: vault.nodes.len(),
        block_count: 0,
        root_url: None,
    }
}

/// The node's directory, relative to the vault root — empty for files
/// directly in the vault root.
fn node_dir(node: &crate::org_parser::Node, vault_dir: &Path) -> PathBuf {
    node.file_path
        .strip_prefix(vault_dir)
        .unwrap_or(&node.file_path)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// `dir` and every ancestor up to (not including) the vault root,
/// shallowest first — e.g. `backend/autopilot` → `[backend,
/// backend/autopilot]`.
fn ancestors_shallowest_first(dir: &Path) -> Vec<PathBuf> {
    let mut chain: Vec<PathBuf> = dir
        .ancestors()
        .filter(|a| !a.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .collect();
    chain.reverse();
    chain
}

/// Create the root page, the directory pages, the blank node pages, the
/// content, and post-validate — everything that talks to Notion.
async fn publish(
    vault: &vault::Vault,
    vault_dir: &Path,
    parent_page_id: &str,
    title: &str,
    concurrency: usize,
    notion: &impl NotionApi,
    reporter: &mut impl Reporter,
) -> Result<RunReport, RunError> {
    reporter.info(&format!("Creating snapshot root page \"{title}\"..."));
    let root = notion
        .create_page(parent_page_id, title)
        .await
        .map_err(api_error("failed to create the snapshot root page", None))?;
    reporter.info(&format!("Root page created: {}", root.url));

    let dir_pages = create_directory_pages(vault, vault_dir, &root, notion, reporter).await?;
    let id_to_page =
        create_node_pages(vault, vault_dir, &dir_pages, &root, notion, reporter).await?;
    let block_count = write_content(
        vault,
        vault_dir,
        &id_to_page,
        &root,
        concurrency,
        notion,
        reporter,
    )
    .await?;
    post_validate_pages(vault, &id_to_page, &root, concurrency, notion, reporter).await?;

    Ok(RunReport {
        node_count: vault.nodes.len(),
        block_count,
        root_url: Some(root.url),
    })
}

/// Wrap a [`NotionError`] into [`RunError::Api`] with the given context.
fn api_error(
    context: impl Into<String>,
    root_url: Option<&str>,
) -> impl FnOnce(crate::ports::NotionError) -> RunError {
    let context = context.into();
    let root_url = root_url.map(str::to_string);
    move |source| RunError::Api {
        context,
        source,
        root_url,
    }
}

/// Create one page per vault subdirectory that (transitively) contains a
/// node, so the page hierarchy mirrors the vault's directory tree.
/// Returns the relative-directory → page-ID map, with the empty path
/// mapped to the root page.
///
/// Creation is deliberately sequential: Notion shows sibling pages in
/// creation order and offers no reorder API, so concurrent creation
/// would scramble the sorted order. Sequential iteration of the
/// `BTreeSet` also guarantees a parent's page exists before its
/// children's (a parent path is a strict component-prefix of its
/// children).
async fn create_directory_pages(
    vault: &vault::Vault,
    vault_dir: &Path,
    root: &crate::ports::CreatedPage,
    notion: &impl NotionApi,
    reporter: &mut impl Reporter,
) -> Result<HashMap<PathBuf, String>, RunError> {
    let dirs: BTreeSet<PathBuf> = vault
        .nodes
        .iter()
        .flat_map(|node| ancestors_shallowest_first(&node_dir(node, vault_dir)))
        .collect();
    reporter.info(&format!("Creating {} directory page(s)...", dirs.len()));
    let mut dir_pages: HashMap<PathBuf, String> = HashMap::new();
    dir_pages.insert(PathBuf::new(), root.id.clone());
    for dir in &dirs {
        let parent = &dir_pages[dir.parent().unwrap_or(Path::new(""))];
        let dir_title = dir.file_name().unwrap_or_default().to_string_lossy();
        let page = notion
            .create_page(parent, &dir_title)
            .await
            .map_err(api_error(
                format!(
                    "failed to create the page for directory \"{}\"",
                    dir.display()
                ),
                Some(&root.url),
            ))?;
        dir_pages.insert(dir.clone(), page.id);
    }
    Ok(dir_pages)
}

/// Pass 1: create every node's page blank (under its directory's page),
/// so the org-ID → page-ID map is complete before any content (and
/// therefore any link mention) is written. Creation is deliberately
/// sequential, in path-sorted node order: Notion shows sibling pages in
/// creation order and offers no reorder API, so concurrent creation
/// would scramble the sorted order. Nodes in a continuous directory get
/// no page of their own: they map to their directory's page, onto which
/// their content is concatenated in pass 2.
async fn create_node_pages(
    vault: &vault::Vault,
    vault_dir: &Path,
    dir_pages: &HashMap<PathBuf, String>,
    root: &crate::ports::CreatedPage,
    notion: &impl NotionApi,
    reporter: &mut impl Reporter,
) -> Result<HashMap<String, String>, RunError> {
    let (merged_nodes, paged_nodes): (Vec<&Node>, Vec<&Node>) = vault
        .nodes
        .iter()
        .partition(|node| vault.continuous_dirs.contains(&node_dir(node, vault_dir)));
    if merged_nodes.is_empty() {
        reporter.info(&format!("Creating {} node page(s)...", paged_nodes.len()));
    } else {
        reporter.info(&format!(
            "Creating {} node page(s) ({} node(s) merged into {} continuous page(s))...",
            paged_nodes.len(),
            merged_nodes.len(),
            vault.continuous_dirs.len()
        ));
    }
    let mut id_to_page: HashMap<String, String> = HashMap::new();
    for node in paged_nodes {
        let parent = &dir_pages[&node_dir(node, vault_dir)];
        let page = notion
            .create_page(parent, &node.title)
            .await
            .map_err(api_error(
                format!("failed to create the page for node {:?}", node.title),
                Some(&root.url),
            ))?;
        id_to_page.insert(node.id.clone(), page.id);
    }
    for node in merged_nodes {
        let dir = node_dir(node, vault_dir);
        id_to_page.insert(node.id.clone(), dir_pages[&dir].clone());
    }
    Ok(id_to_page)
}

/// Pass 2: convert and write each node's content, links rewritten into
/// page mentions via the now-complete map. Writes are buffered across
/// pages but sequential within a page: chunk order matters, and nodes
/// sharing a continuous page must land in node order — path-sorted, so a
/// directory's files concatenate in file-name order — each introduced by
/// its title as a heading (the title otherwise lives only in the page
/// name).
async fn write_content(
    vault: &vault::Vault,
    vault_dir: &Path,
    id_to_page: &HashMap<String, String>,
    root: &crate::ports::CreatedPage,
    concurrency: usize,
    notion: &impl NotionApi,
    reporter: &mut impl Reporter,
) -> Result<usize, RunError> {
    reporter.info("Writing content...");
    let converter = Converter::new().with_notion_transformer(anchors_to_bold);

    // Group nodes by target page, preserving node order within each group.
    let mut groups: Vec<(&String, Vec<&Node>)> = Vec::new();
    let mut group_of: HashMap<&String, usize> = HashMap::new();
    for node in &vault.nodes {
        let page_id = &id_to_page[&node.id];
        if let Some(&i) = group_of.get(page_id) {
            groups[i].1.push(node);
        } else {
            group_of.insert(page_id, groups.len());
            groups.push((page_id, vec![node]));
        }
    }

    let counts: Vec<usize> = stream::iter(&groups)
        .map(|(page_id, nodes)| {
            let converter = &converter;
            async move {
                let mut count = 0usize;
                for node in nodes {
                    let mut blocks = if vault.continuous_dirs.contains(&node_dir(node, vault_dir)) {
                        let mut with_title = vec![OrgBlock::Heading {
                            level: 1,
                            spans: vec![Span::Text(node.title.clone())],
                        }];
                        with_title.extend(node.blocks.iter().cloned());
                        converter.convert(&with_title, id_to_page)
                    } else {
                        converter.convert(&node.blocks, id_to_page)
                    };
                    if node.has_tag("unreviewed") {
                        blocks = unreviewed_banner(blocks);
                    }
                    count += blocks.len();
                    append_children_chunked(notion, page_id, &blocks)
                        .await
                        .map_err(api_error(
                            format!("failed to write content for node {:?}", node.title),
                            Some(&root.url),
                        ))?;
                }
                Ok::<_, RunError>(count)
            }
        })
        .buffered(concurrency)
        .try_collect()
        .await?;
    Ok(counts.iter().sum())
}

/// Post-validation: read every page back and confirm the expected
/// mentions actually landed. Each unique page is fetched once, buffered
/// `concurrency`-wide — nodes merged onto a continuous page share the
/// fetch.
async fn post_validate_pages(
    vault: &vault::Vault,
    id_to_page: &HashMap<String, String>,
    root: &crate::ports::CreatedPage,
    concurrency: usize,
    notion: &impl NotionApi,
    reporter: &mut impl Reporter,
) -> Result<(), RunError> {
    reporter.info("Post-validating...");
    // Unique pages, each paired with the first node on it (for error
    // context).
    let mut seen: HashSet<&String> = HashSet::new();
    let pages: Vec<(&String, &Node)> = vault
        .nodes
        .iter()
        .filter_map(|node| {
            let page_id = &id_to_page[&node.id];
            seen.insert(page_id).then_some((page_id, node))
        })
        .collect();
    let fetched: HashMap<&String, HashSet<String>> = stream::iter(pages)
        .map(|(page_id, node)| async move {
            let found = validate::mention_page_ids(notion, page_id)
                .await
                .map_err(api_error(
                    format!("post-validation read failed for node {:?}", node.title),
                    Some(&root.url),
                ))?;
            Ok::<_, RunError>((page_id, found))
        })
        .buffered(concurrency)
        .try_collect()
        .await?;

    let failures: Vec<_> = vault
        .nodes
        .iter()
        .map(|node| validate::check_node(node, id_to_page, &fetched[&id_to_page[&node.id]]))
        .filter(|result| !result.passed())
        .collect();
    if failures.is_empty() {
        Ok(())
    } else {
        Err(RunError::PostValidation {
            root_url: root.url.clone(),
            failures,
        })
    }
}
