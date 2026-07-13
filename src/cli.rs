//! Rendering of run failures for the terminal, kept in the library (pure
//! string/exit-code logic) so it is unit-testable; `main` only prints.

use crate::run::RunError;
use std::fmt::Write;

/// Exit code for a [`RunError`]: 2 for pre-validation failures, 3 for
/// post-validation failures, 1 for everything else.
#[must_use]
pub fn exit_code_for(error: &RunError) -> u8 {
    match error {
        RunError::PreValidation(_) => 2,
        RunError::PostValidation { .. } => 3,
        _ => 1,
    }
}

/// Render a run failure as the full multi-line message for stderr,
/// including per-link details and the partial-snapshot warning where a
/// root page had already been created.
#[must_use]
pub fn render_failure(error: &RunError) -> String {
    let mut out = format!("error: {error}");
    match error {
        RunError::PreValidation(broken) => {
            for b in broken {
                let _ = write!(out, "\n  - {b}");
            }
        }
        RunError::PostValidation { root_url, failures } => {
            for f in failures {
                let _ = write!(
                    out,
                    "\n  - node {}: missing mention(s) for {:?}",
                    f.node_id, f.missing
                );
            }
            out.push_str(&partial_snapshot_warning(root_url));
        }
        RunError::Api {
            root_url: Some(url),
            ..
        } => out.push_str(&partial_snapshot_warning(url)),
        _ => {}
    }
    out
}

fn partial_snapshot_warning(root_url: &str) -> String {
    format!(
        "\n\nWARNING: this snapshot is incomplete or failed validation.\n\
         It was NOT automatically deleted. Root page: {root_url}\n\
         Delete it manually in Notion, then re-run once the issue is fixed."
    )
}
