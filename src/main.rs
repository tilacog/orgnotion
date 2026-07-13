//! CLI entry point: parses arguments, wires the real adapters into the
//! run orchestration, and maps outcomes to exit codes.

use clap::Parser;
use orgnotion::adapters::{NotionrsApi, ProcessEnv, RealFileSystem, StdoutReporter, SystemClock};
use orgnotion::cli::{exit_code_for, render_failure};
use orgnotion::ports::Env;
use orgnotion::run::{self, RunConfig, RunError, TOKEN_ENV_VAR};
use std::path::PathBuf;
use std::process::ExitCode;

/// Publish an Org-roam vault as a fresh, read-only snapshot in Notion.
///
/// Every invocation creates a brand-new root page in Notion and writes the
/// entire vault underneath it as child pages. Snapshots from previous runs
/// are never touched — Notion is a disposable mirror, and the local vault
/// is always the source of truth.
#[derive(Parser, Debug)]
#[command(name = "orgnotion", version, about)]
struct Args {
    /// Path to the org-roam vault directory to publish.
    vault_dir: PathBuf,

    /// Notion page ID to create the new snapshot root page under (the
    /// page must be shared with your integration). Falls back to the
    /// `ORGNOTION_PARENT_PAGE_ID` environment variable.
    #[arg(long)]
    parent_page_id: Option<String>,

    /// Title for the snapshot root page. Defaults to
    /// "Org-roam snapshot <ISO-8601 timestamp>".
    #[arg(long)]
    title: Option<String>,

    /// Scan and pre-validate the vault only; print what would be created
    /// and write nothing to Notion.
    #[arg(long)]
    dry_run: bool,

    /// Maximum number of Notion API calls in flight at once, for content
    /// writes and validation reads (page creation is sequential to keep
    /// sibling pages in sorted order). Notion's rate limit averages
    /// ~3 requests/second; 429s are retried with backoff.
    #[arg(long, default_value_t = 4)]
    concurrency: usize,
}

/// Exit codes: 0 success, 1 generic/API failure, 2 pre-validation
/// failure, 3 post-validation failure.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = Args::parse();
    let config = RunConfig {
        vault_dir: args.vault_dir,
        parent_page_id: args.parent_page_id,
        title: args.title,
        dry_run: args.dry_run,
        concurrency: args.concurrency.max(1),
    };

    let env = ProcessEnv;
    let token = if config.dry_run {
        Some(String::new()) // dry runs never contact Notion
    } else {
        env.var(TOKEN_ENV_VAR)
    };
    let Some(token) = token else {
        eprintln!("error: {}", RunError::MissingToken);
        return ExitCode::FAILURE;
    };

    let outcome = run::execute(
        &config,
        &RealFileSystem,
        &NotionrsApi::new(token),
        &SystemClock::new(),
        &env,
        &mut StdoutReporter,
    )
    .await;

    match outcome {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{}", render_failure(&e));
            ExitCode::from(exit_code_for(&e))
        }
    }
}
