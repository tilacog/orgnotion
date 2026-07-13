# orgnotion

Publish an [Org-roam](https://www.orgroam.com/) vault as a fresh,
read-only snapshot in [Notion](https://www.notion.com/).

Each run creates a new root page and mirrors the vault's directory tree
under it — one page per directory, one page per node (`.org` file), with
`[[id:...]]` links rewritten into Notion page mentions. Runs are
self-contained: nothing from previous runs is touched, no state is kept.
Only directories that (transitively) contain `.org` files get a page.

```
<parent page (yours, shared with the integration)>
└── Org-roam snapshot 2026-07-12T14:03:00Z
    ├── Node A
    ├── backend
    │   ├── Node B
    │   └── indexer
    │       └── Node C
    └── ...
```

**Index nodes.** An `.INDEX` file in a directory names one of its
`.org` files as the directory's index node and picks a layout:

```
index.org
flat = true
```

The index node's content is rendered directly on the directory's page,
which takes the node's `#+TITLE:` instead of the directory basename
(subdirectories still nest under it). With `flat = true`, the
directory's other `.org` files (non-recursive) are concatenated onto
that same page after the index node, in file-name order, each
introduced by its `#+TITLE:` as a Heading 1; links to any of them
mention the shared page. With `flat = false`, they keep their own child
pages as usual. Both lines are required, in any order; blank lines are
ignored. At the vault root, the index content lands on the snapshot
root page itself (which keeps its own title).

**Unreviewed nodes.** A node tagged `:unreviewed:` (via `#+filetags:`)
gets a red-text ⚠️ callout as its first block, warning that the content
hasn't been reviewed.

**Validation.** Pre-validation: every `[[id:...]]` link must resolve
within the vault, or the run aborts before writing anything.
Post-validation: every page is read back and each expected mention is
checked against its target page ID; mismatches flag the snapshot as
invalid. Partial/invalid snapshots are not deleted automatically — the
CLI prints the root URL; delete it in Notion and re-run.

## Usage

```sh
cargo install --path .
export NOTION_TOKEN=secret_...   # internal integration; share the parent page with it
orgnotion <VAULT_DIR> --parent-page-id <ID>
```

- `--parent-page-id <ID>` — falls back to `ORGNOTION_PARENT_PAGE_ID`; required.
- `--title <STRING>` — root page title. Default: `Org-roam snapshot <timestamp>`.
- `--dry-run` — scan + pre-validate only; write nothing to Notion.
- `--concurrency <N>` — max API calls in flight (default 4), for content
  writes and validation reads. Page creation is always sequential: Notion
  shows sibling pages in creation order and has no reorder API, so this
  is what keeps them sorted.

Exit codes: `0` published and validated, `1` usage/token/API failure,
`2` pre-validation failure (nothing written), `3` post-validation
failure (snapshot flagged invalid).

## Architecture

All I/O sits behind traits in `src/ports.rs` (`FileSystem`, `NotionApi`,
`Clock`, `Env`, `Reporter`); real implementations in `src/adapters/`,
wired in `main.rs`. `run.rs` orchestrates: scan (`vault.rs`, parsing via
[`orgize`](https://crates.io/crates/orgize) in `org_parser.rs`) →
pre-validate → create root, directory, and blank node pages → convert
(`converter.rs`) and write content → post-validate (`validate.rs`).
Tests inject in-memory fakes (`tests/common/mod.rs`); the adapter's HTTP
path is tested against an in-process stub server.

Notes:

- Notion API `2026-03-11` via [`notionrs`](https://crates.io/crates/notionrs)
  / `notionrs_types` (typed blocks end to end); three endpoints:
  `POST /pages`, `PATCH /blocks/{id}/children`, `GET /blocks/{id}/children`.
- Retries live in the adapter: 5 attempts on 429/5xx, exponential
  backoff from 300 ms. `list_children` bypasses notionrs 0.28.0's
  `get_block_children`, which drops the pagination cursor.
- One page per file: headline-level `:ID:` drawers are not split out;
  duplicate IDs across files abort the scan. Unrecognized org constructs
  degrade to plain paragraphs.

## Development

```sh
cargo test && cargo fmt --check && cargo clippy --all-targets --all-features
cargo llvm-cov --lcov --output-path lcov.info && cargo crap --lcov lcov.info --fail-above
ln -sf ../../scripts/pre-commit .git/hooks/pre-commit   # runs all of the above
```

The [`cargo-crap`](https://github.com/minikin/cargo-crap) gate
(threshold 25, `.cargo-crap.toml`) flags complex-and-under-tested
functions; it needs `rustup component add llvm-tools-preview` and
`cargo install cargo-llvm-cov cargo-crap`.
