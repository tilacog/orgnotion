//! Pre- and post-validation of the vault's internal link structure.
//!
//! Pre-validation runs before any Notion write and guarantees every
//! `[[id:...]]` link resolves within the vault. Post-validation runs
//! after writing and confirms the mentions Notion actually stored point
//! at the expected target pages.

use crate::converter::{LinkTarget, rich_text_of};
use crate::notion::fetch_all_children_recursive;
use crate::org_parser::Node;
use crate::ports::{ChildBlock, NotionApi, NotionError};
use crate::vault::Vault;
use notionrs_types::object::rich_text::{RichText, mention::Mention};
use std::collections::{HashMap, HashSet};

/// One `[[id:...]]` link whose target does not exist in the vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenLink {
    /// Org-roam ID of the node containing the link.
    pub source_node_id: String,
    /// File the link appears in.
    pub source_file: String,
    /// The ID the link points at, which no node in the vault has.
    pub target_id: String,
}

impl std::fmt::Display for BrokenLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} (node {}) links to unknown id [[id:{}]]",
            self.source_file, self.source_node_id, self.target_id
        )
    }
}

/// Check that every link in every node resolves to a node that exists in
/// the vault.
///
/// # Errors
///
/// Returns *all* broken links found (not just the first) so the user can
/// fix everything in one pass.
pub fn pre_validate(vault: &Vault) -> Result<(), Vec<BrokenLink>> {
    let index = vault.id_index();
    let mut broken = Vec::new();

    for node in &vault.nodes {
        for target in &node.links {
            if !index.contains_key(target.as_str()) {
                broken.push(BrokenLink {
                    source_node_id: node.id.clone(),
                    source_file: node.file_path.display().to_string(),
                    target_id: target.clone(),
                });
            }
        }
    }

    if broken.is_empty() {
        Ok(())
    } else {
        Err(broken)
    }
}

/// Post-validation outcome for one node page.
#[derive(Debug)]
pub struct PostValidationResult {
    /// The node's org-roam ID.
    pub node_id: String,
    /// Org-roam IDs whose expected page mention was not found in Notion.
    pub missing: Vec<String>,
}

impl PostValidationResult {
    /// Whether every expected mention was found.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.missing.is_empty()
    }
}

/// After a node's content has been written to Notion, fetch it back and
/// confirm every expected link is present — as a page mention for paged
/// nodes, or as a text-link URL for merged nodes — pointing at the right
/// target.
///
/// # Errors
///
/// Propagates Notion API failures encountered while reading the page
/// back; validation mismatches are reported in the `Ok` result, not as
/// errors.
// implicit_hasher: application-internal API; callers always pass the
// std default-hashed map built by the run orchestration.
#[allow(clippy::implicit_hasher)]
pub async fn post_validate(
    notion: &impl NotionApi,
    node: &Node,
    id_to_link: &HashMap<String, LinkTarget>,
) -> Result<PostValidationResult, NotionError> {
    let Some(target) = id_to_link.get(&node.id) else {
        // Unreachable if the run created a page per node, but don't panic
        // in a validation path — report every link as missing instead.
        return Ok(PostValidationResult {
            node_id: node.id.clone(),
            missing: node.links.clone(),
        });
    };
    let page_id = match target {
        LinkTarget::Page(pid) => pid,
        LinkTarget::Block { page_id, .. } => page_id,
    };
    let found = mention_page_ids(notion, page_id).await?;
    Ok(check_node(node, id_to_link, &found))
}

/// Fetch a page's content back from Notion and collect every link
/// reference it carries: page-mention IDs and text-link URLs. Callers
/// validating several nodes that share one page (a directory with an
/// index) fetch once and run [`check_node`] per node.
///
/// # Errors
///
/// Propagates Notion API failures encountered while reading the page.
pub async fn mention_page_ids(
    notion: &impl NotionApi,
    page_id: &str,
) -> Result<FoundLinks, NotionError> {
    let children = fetch_all_children_recursive(notion, page_id).await?;
    Ok(collect_links(&children))
}

/// Check one node's expected links against the page-mention IDs and
/// link URLs actually `found` on its page.
// implicit_hasher: see `post_validate`.
#[allow(clippy::implicit_hasher)]
#[must_use]
pub fn check_node(
    node: &Node,
    id_to_link: &HashMap<String, LinkTarget>,
    found: &FoundLinks,
) -> PostValidationResult {
    let missing = node
        .links
        .iter()
        .filter(|target| {
            match id_to_link.get(*target) {
                Some(LinkTarget::Page(page_id)) => !found.page_ids.contains(page_id),
                Some(LinkTarget::Block { url, .. }) => !found.link_urls.contains(url),
                None => true,
            }
        })
        .cloned()
        .collect();

    PostValidationResult {
        node_id: node.id.clone(),
        missing,
    }
}

/// All link references found on a page: page-mention IDs (from `@page`
/// mentions) and text-link URLs (from inline `href` fields).
#[derive(Debug, Default, Clone)]
pub struct FoundLinks {
    /// Notion page IDs from page mentions.
    pub page_ids: HashSet<String>,
    /// URLs from text-link `href` fields (block-level links).
    pub link_urls: HashSet<String>,
}

/// Walk the fetched blocks and collect every link reference: page-mention
/// IDs and text-link `href` URLs.
fn collect_links(blocks: &[ChildBlock]) -> FoundLinks {
    let mut found = FoundLinks::default();
    for child in blocks {
        let Some(rich_text) = rich_text_of(&child.block) else {
            continue;
        };
        for rt in rich_text {
            match rt {
                RichText::Mention {
                    mention: Mention::Page { page },
                    ..
                } => {
                    found.page_ids.insert(page.id.clone());
                }
                RichText::Text { text, .. } => {
                    if let Some(link) = &text.link {
                        found.link_urls.insert(link.url.clone());
                    }
                }
                _ => {}
            }
        }
    }
    found
}
