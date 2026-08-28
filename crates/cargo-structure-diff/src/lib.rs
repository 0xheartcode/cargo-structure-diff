//! The M1 capstone: wire extraction, diff, render, and lint into one gate.
//!
//! `csd diff --base <ref>` materializes the base ref in a throwaway git worktree, extracts it,
//! extracts the current working tree as head, diffs the graphs, renders each enabled view (the
//! module `flowchart` and, when `views.enabled` includes `states`, the `stateDiagram-v2`), and runs
//! the architectural lints. A denied finding prints the findings and the diagrams (the diagram is
//! the error message) and exits non-zero; otherwise it exits zero.
//!
//! The real work lives in [`analyze`] so both binaries stay thin and the pipeline is testable
//! without touching the process working directory.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{bail, Context, Result};
use csd_config::Config;
use csd_diff::{diff, member_diff, DiffOptions, FileRename};
use csd_ir::{Change, EdgeKind, Graph, NodeKind, StableId};
use csd_lint::{has_denials, lint, Finding, Severity};
use csd_render::{render, Format, RenderOpts, View};

/// Default base ref when `--base` is omitted.
const DEFAULT_BASE: &str = "main";

/// Default head ref for `walk` when `--head` is omitted.
const DEFAULT_HEAD: &str = "HEAD";

/// Default view for `walk` when `--view` is omitted.
const DEFAULT_VIEW: &str = "modules";

/// Default trace command when `--cmd` is omitted.
const DEFAULT_TRACE_CMD: &str = "cargo test";

/// Usage text for `-h`/`--help` and parse errors.
const HELP: &str = "\
cargo-structure-diff: structural diff and architectural lint gate

USAGE:
    csd diff [--base <ref>|--baseline <file>] [--show] [--full] [--list] [--scope <glob>]... [--format <fmt>]
    csd snapshot [-o <file>|--out <file>]
    csd changelog [--base <ref>]
    csd drift [--base <ref>|--baseline <file>]
    csd files [-o <file>|--out <file>]
    csd init [-o <file>|--out <file>]
    csd walk [--base <ref>] [--head <ref>] [--view <name>] [--full] [--format <fmt>]
    csd trace [--base <ref>] [--cmd <shell command>]
    csd doc [-o <file>|--out <file>]
    cargo structure-diff diff [--base <ref>] [--show]

OPTIONS:
    --base <ref>       Base git ref to diff against (default: main)
    --baseline <file>  diff: use a JSON snapshot as the base instead of a git ref
    --show             diff: print every rendered view even when nothing denies
    --full             diff: render the whole graph, not just the pruned delta
    --list             diff: print a one-line-per-change textual summary of the delta
    --scope <glob>     diff: only render nodes whose source path matches <glob>
                       (repeatable; crossing edges show an external stub)
    --format <fmt>     diff: diagram syntax, one of mermaid|dot|ascii|boxes|svg (default: mermaid)
    --head <ref>       walk: ref that ends the range, inclusive (default: HEAD)
    --view <name>      walk: view to render each frame (default: modules)
    --cmd <cmd>        trace: shell command that emits Mermaid (default: cargo test)
    -o, --out <f>      doc/snapshot: write output to <file> instead of stdout
    -h, --help         Print this help
    -V, --version      Print the version and build commit

trace mode is opt-in and observational: it runs <cmd> in the base and head
worktrees, extracts the emitted `sequenceDiagram` Mermaid blocks, and diffs the
observed traces as a set. It exits 0 (2 only if the command fails to run).

doc mode is observational: it extracts the current working tree and emits a
self-contained Markdown structure report (one Mermaid diagram per enabled view,
plus a module index). No base, no delta. It exits 0 on success, 2 on error.

changelog mode is observational: it diffs the working tree against <ref> and
emits a grouped Markdown changelog (added, removed, modified, and moved items per
module, then dependency edges). It is the textual companion to the diagram, for a
PR body or release note. No gate. It exits 0 on success, 2 on error.

snapshot mode writes the current working tree as a JSON structure graph: a pinned
baseline. Commit it at a release, then gate later work with `csd diff --baseline
<file>` to assert against that structure without checking out the old ref. It
exits 0 on success, 2 on error.

files mode is observational: it prints a per-file structural index of the working
tree, listing for each source file the modules, types (with their members), and
functions it defines, plus the trait realizations declared in it. No base, no
delta. It exits 0 on success, 2 on error.

init writes a starter .csd.toml pre-filled with the crate path globs detected in
this repo, so the layer mapping matches real modules instead of guessed paths.
With no --out it writes .csd.toml and refuses to overwrite an existing one; pass
--out - to print to stdout. It exits 0 on success, 2 on error.

drift mode is observational: it prints a compact count of how much the working
tree structure has changed since a base ref (or a --baseline snapshot), broken
down by nodes and edges added, removed, modified, and moved. It answers how far
the structure has moved since the release, without a diagram. It exits 0 on
success, 2 on error.

walk mode renders one frame per first-parent commit in <base>..<head>, oldest to
newest, so you can flip through how the architecture evolved. By default each
frame is the delta from its parent; --full draws the whole graph at each commit.
Pairs well with --format ascii for an in-terminal flip-through. It exits 0 on
success, 2 on error.
";

/// A parsed invocation.
#[derive(Debug, PartialEq, Eq)]
pub enum Cmd {
    /// Print usage and exit zero.
    Help,
    /// Print the version (with the build's git commit) and exit zero.
    Version,
    /// Run the diff gate against `base`.
    Diff {
        /// The git ref to treat as the base side.
        base: String,
        /// Diff against this pinned JSON snapshot instead of materializing `base` from git.
        baseline: Option<String>,
        /// Print every rendered view even when nothing denies (for interactive use).
        show: bool,
        /// Render the whole graph, not just the pruned delta.
        full: bool,
        /// Path globs; only nodes whose source path matches render (empty means all).
        scope: Vec<String>,
        /// Print a deterministic one-line-per-change textual summary of the delta.
        list: bool,
        /// Diagram output syntax.
        format: Format,
    },
    /// Serialize the current working tree as a JSON structure snapshot (a pinned baseline).
    Snapshot {
        /// Output file; `None` writes to stdout.
        out: Option<String>,
    },
    /// Run a trace command in base and head, then diff the observed Mermaid traces.
    Trace {
        /// The git ref to treat as the base side.
        base: String,
        /// The shell command that emits Mermaid on stdout.
        cmd: String,
    },
    /// Emit a whole-codebase structure report (snapshot of head, no delta).
    Doc {
        /// Output file; `None` writes to stdout.
        out: Option<String>,
    },
    /// Emit a grouped Markdown changelog of the base-to-head delta (no gate, no diagram).
    Changelog {
        /// The git ref to treat as the base side.
        base: String,
    },
    /// Emit a per-file structural index of the working tree (no delta, no diagram).
    Files {
        /// Output file; `None` writes to stdout.
        out: Option<String>,
    },
    /// Generate a starter `.csd.toml` pre-filled with the repo's detected crate globs.
    Init {
        /// Output file; `None` writes `.csd.toml` (refusing to overwrite), `-` writes stdout.
        out: Option<String>,
    },
    /// Summarize how far the working tree has drifted from a base ref or a pinned baseline.
    Drift {
        /// The git ref to measure drift from (ignored when `baseline` is set).
        base: String,
        /// Measure drift from this JSON snapshot instead of a git ref.
        baseline: Option<String>,
    },
    /// Render one frame per first-parent commit across a range, oldest to newest.
    Walk {
        /// The git ref that begins the range (exclusive), the parent of the first frame.
        base: String,
        /// The git ref that ends the range (inclusive). Defaults to `HEAD`.
        head: String,
        /// The view to render each frame in (for example `modules`).
        view: String,
        /// Render the whole graph at each commit instead of the delta from its parent.
        full: bool,
        /// Diagram output syntax.
        format: Format,
    },
}

/// One rendered view: its name and the Mermaid source.
pub struct ViewDiagram {
    /// The view name (`modules`, `states`).
    pub view: String,
    /// The rendered Mermaid diagram.
    pub mermaid: String,
}

/// The result of a run, kept separate from printing so tests can assert on it.
pub struct Report {
    /// The base-to-head delta.
    pub changes: Vec<Change>,
    /// The lint findings (sorted, deny and warn).
    pub findings: Vec<Finding>,
    /// The rendered diagrams, one per enabled view.
    pub diagrams: Vec<ViewDiagram>,
}

impl Report {
    /// Whether any finding denies the build.
    pub fn denied(&self) -> bool {
        has_denials(&self.findings)
    }

    /// The process exit code: 1 on a denied finding, else 0.
    pub fn exit_code(&self) -> i32 {
        i32::from(self.denied())
    }

    /// Print the always-on summary. On a denial, the findings and diagrams go to stderr (the
    /// diagram is the error message). With `show`, the diagrams are also printed on a clean run,
    /// to stdout, so the tool is usable interactively.
    fn print(&self, base: &str, show: bool) {
        let denials = self
            .findings
            .iter()
            .filter(|f| f.severity == Severity::Deny)
            .count();
        let warns = self.findings.len() - denials;
        println!(
            "cargo-structure-diff: {} change(s) vs {base}, {denials} denial(s), {warns} warning(s)",
            self.changes.len()
        );
        let denied = self.denied();
        if denied {
            eprintln!("\nlint findings:");
            for f in &self.findings {
                let tag = match f.severity {
                    Severity::Deny => "deny",
                    Severity::Warn => "warn",
                };
                eprintln!("  [{tag}] {}: {}", f.rule, f.message);
            }
        }
        for d in &self.diagrams {
            if denied {
                eprintln!("\n{} view:\n{}", d.view, d.mermaid);
            } else if show {
                println!("\n{} view:\n{}", d.view, d.mermaid);
            }
        }
    }
}

/// Parse the argument list (already stripped of the program name).
///
/// Accepts the `diff` and `trace` subcommands with an optional `--base <ref>` or `--base=<ref>`,
/// `trace` also taking `--cmd <shell command>`, and `-h`/`--help` anywhere. Dependency-free by
/// design; no clap.
pub fn parse_args(args: &[String]) -> Result<Cmd> {
    let mut base = DEFAULT_BASE.to_string();
    let mut head = DEFAULT_HEAD.to_string();
    let mut view = DEFAULT_VIEW.to_string();
    let mut baseline: Option<String> = None;
    let mut cmd = DEFAULT_TRACE_CMD.to_string();
    let mut show = false;
    let mut full = false;
    let mut list = false;
    let mut scope: Vec<String> = Vec::new();
    let mut format = Format::default();
    let mut out: Option<String> = None;
    let mut sub: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-h" | "--help" => return Ok(Cmd::Help),
            "-V" | "--version" => return Ok(Cmd::Version),
            "--show" => show = true,
            "--full" => full = true,
            "--list" => list = true,
            "diff" | "trace" | "doc" | "changelog" | "snapshot" | "files" | "init" | "walk"
            | "drift"
                if sub.is_none() =>
            {
                sub = Some(arg)
            }
            "--baseline" => {
                i += 1;
                let value = args.get(i).context("--baseline requires a value")?;
                baseline = Some(value.clone());
            }
            _ if arg.starts_with("--baseline=") => {
                baseline = Some(arg["--baseline=".len()..].to_string());
            }
            "-o" | "--out" => {
                i += 1;
                let value = args.get(i).context("--out requires a value")?;
                out = Some(value.clone());
            }
            _ if arg.starts_with("--out=") => {
                out = Some(arg["--out=".len()..].to_string());
            }
            "--base" => {
                i += 1;
                let value = args.get(i).context("--base requires a value")?;
                base = value.clone();
            }
            _ if arg.starts_with("--base=") => {
                base = arg["--base=".len()..].to_string();
            }
            "--head" => {
                i += 1;
                let value = args.get(i).context("--head requires a value")?;
                head = value.clone();
            }
            _ if arg.starts_with("--head=") => {
                head = arg["--head=".len()..].to_string();
            }
            "--view" => {
                i += 1;
                let value = args.get(i).context("--view requires a value")?;
                view = value.clone();
            }
            _ if arg.starts_with("--view=") => {
                view = arg["--view=".len()..].to_string();
            }
            "--scope" => {
                i += 1;
                let value = args.get(i).context("--scope requires a value")?;
                scope.push(value.clone());
            }
            _ if arg.starts_with("--scope=") => {
                scope.push(arg["--scope=".len()..].to_string());
            }
            "--format" => {
                i += 1;
                let value = args.get(i).context("--format requires a value")?;
                format = parse_format(value)?;
            }
            _ if arg.starts_with("--format=") => {
                format = parse_format(&arg["--format=".len()..])?;
            }
            "--cmd" => {
                i += 1;
                let value = args.get(i).context("--cmd requires a value")?;
                cmd = value.clone();
            }
            _ if arg.starts_with("--cmd=") => {
                cmd = arg["--cmd=".len()..].to_string();
            }
            other => bail!("unexpected argument {other:?}; try --help"),
        }
        i += 1;
    }
    match sub {
        Some("trace") => Ok(Cmd::Trace { base, cmd }),
        Some("doc") => Ok(Cmd::Doc { out }),
        Some("snapshot") => Ok(Cmd::Snapshot { out }),
        Some("files") => Ok(Cmd::Files { out }),
        Some("init") => Ok(Cmd::Init { out }),
        Some("walk") => Ok(Cmd::Walk {
            base,
            head,
            view,
            full,
            format,
        }),
        Some("changelog") => Ok(Cmd::Changelog { base }),
        Some("drift") => Ok(Cmd::Drift { base, baseline }),
        Some(_) => Ok(Cmd::Diff {
            base,
            baseline,
            show,
            full,
            scope,
            list,
            format,
        }),
        None => bail!(
            "expected one of the `diff`, `changelog`, `snapshot`, `trace`, or `doc` subcommands; try --help"
        ),
    }
}

/// Parse a `--format` value into a [`Format`], erroring on an unknown syntax.
fn parse_format(value: &str) -> Result<Format> {
    match value {
        "mermaid" => Ok(Format::Mermaid),
        "dot" => Ok(Format::Dot),
        "ascii" => Ok(Format::Ascii),
        "boxes" => Ok(Format::Boxes),
        "svg" => Ok(Format::Svg),
        other => bail!("unknown --format {other:?}; expected mermaid, dot, ascii, boxes, or svg"),
    }
}

/// The version line: the package version plus the build's git commit and its (deterministic) commit
/// timestamp when available, for example `csd 0.0.0 (a678f58 2026-08-28T12:19:01Z)`. Falls back to
/// the bare version when the crate was built without a git checkout (a crates.io tarball).
fn version_string() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let sha = env!("CSD_GIT_SHA");
    let date = env!("CSD_GIT_DATE");
    match (sha.is_empty(), date.is_empty()) {
        (true, _) => format!("csd {version}"),
        (false, true) => format!("csd {version} ({sha})"),
        (false, false) => format!("csd {version} ({sha} {date})"),
    }
}

/// Entry point shared by both binaries. Returns the process exit code.
pub fn run(args: &[String]) -> i32 {
    let cmd = match parse_args(args) {
        Ok(cmd) => cmd,
        Err(e) => {
            eprintln!("error: {e}\n");
            eprint!("{HELP}");
            return 2;
        }
    };
    match cmd {
        Cmd::Help => {
            print!("{HELP}");
            0
        }
        Cmd::Version => {
            println!("{}", version_string());
            0
        }
        Cmd::Diff {
            base,
            baseline,
            show,
            full,
            scope,
            list,
            format,
        } => {
            let opts = RenderOpts {
                full,
                scope,
                entry: None,
                format,
            };
            match run_diff(&base, baseline.as_deref(), show, list, &opts) {
                Ok(code) => code,
                Err(e) => {
                    eprintln!("error: {e:#}");
                    2
                }
            }
        }
        Cmd::Snapshot { out } => match run_snapshot(out.as_deref()) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e:#}");
                2
            }
        },
        Cmd::Files { out } => match run_files(out.as_deref()) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e:#}");
                2
            }
        },
        Cmd::Init { out } => match run_init(out.as_deref()) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e:#}");
                2
            }
        },
        Cmd::Trace { base, cmd } => match run_trace(&base, &cmd) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e:#}");
                2
            }
        },
        Cmd::Doc { out } => match run_doc(out.as_deref()) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e:#}");
                2
            }
        },
        Cmd::Changelog { base } => match run_changelog(&base) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e:#}");
                2
            }
        },
        Cmd::Walk {
            base,
            head,
            view,
            full,
            format,
        } => match run_walk(&base, &head, &view, full, format) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e:#}");
                2
            }
        },
        Cmd::Drift { base, baseline } => match run_drift(&base, baseline.as_deref()) {
            Ok(code) => code,
            Err(e) => {
                eprintln!("error: {e:#}");
                2
            }
        },
    }
}

/// Resolve the repo root, load config, run the pipeline, and print the report.
///
/// With `list`, a deterministic one-line-per-change summary is printed to stdout before the
/// diagrams; it composes with `show` and does not affect the exit code.
fn run_diff(
    base: &str,
    baseline: Option<&str>,
    show: bool,
    list: bool,
    opts: &RenderOpts,
) -> Result<i32> {
    let repo = repo_root()?;
    let config = load_config(&repo)?;
    // The label printed in the summary: the snapshot file when pinned, else the git ref.
    let (report, label) = match baseline {
        Some(path) => {
            let graph = load_baseline(path)?;
            (analyze_baseline(&repo, &graph, &config, opts)?, path)
        }
        None => (analyze(&repo, base, &config, opts)?, base),
    };
    report.print(label, show);
    if list {
        for line in change_list(&report.changes) {
            println!("{line}");
        }
    }
    Ok(report.exit_code())
}

/// Write the working-tree structure snapshot to `out`, or stdout when `out` is `None`.
fn run_snapshot(out: Option<&str>) -> Result<i32> {
    let repo = repo_root()?;
    let json = snapshot_json(&repo)?;
    match out {
        Some(path) => std::fs::write(path, &json)
            .with_context(|| format!("failed to write snapshot {path}"))?,
        None => println!("{json}"),
    }
    Ok(0)
}

/// Format the delta as a deterministic, reviewer-friendly change list, one line per change.
///
/// The order follows `csd_diff::diff`'s stable sort (added nodes, removed, modified, moved, then
/// edge add/remove), so the list is byte-stable across runs. See [`change_line`] for the formats.
fn change_list(changes: &[Change]) -> Vec<String> {
    changes.iter().map(change_line).collect()
}

/// One textual line for a single [`Change`] (lowercase kinds).
///
/// - `+ <kind> <id>` / `- <kind> <id>` for an added / removed node.
/// - `~ <kind> <id>` for a modified node, with `(+f ~g -h)` member detail appended when the
///   before/after fingerprints differ (added `+`, changed `~`, removed `-`).
/// - `> moved <id> from <from> to <to>` for a re-parented node.
/// - `+ edge <from> -> <to> (<kind>)` / `- edge ...` for an added / removed edge.
fn change_line(change: &Change) -> String {
    match change {
        Change::Added(n) => format!("+ {} {}", node_kind_name(n.kind), n.id.as_str()),
        Change::Removed(n) => format!("- {} {}", node_kind_name(n.kind), n.id.as_str()),
        Change::Modified { before, after } => {
            let head = format!("~ {} {}", node_kind_name(after.kind), after.id.as_str());
            match member_detail(before, after) {
                Some(detail) => format!("{head} {detail}"),
                None => head,
            }
        }
        Change::Moved { node, from, to } => format!(
            "> moved {} from {} to {}",
            node.as_str(),
            from.as_str(),
            to.as_str()
        ),
        Change::EdgeAdded(e) => format!(
            "+ edge {} -> {} ({})",
            e.from.as_str(),
            e.to.as_str(),
            edge_kind_name(e.kind)
        ),
        Change::EdgeRemoved(e) => format!(
            "- edge {} -> {} ({})",
            e.from.as_str(),
            e.to.as_str(),
            edge_kind_name(e.kind)
        ),
    }
}

/// The `(+f ~g -h)` per-member detail for a modified node, or `None` when nothing differs at the
/// member level (a bare `~` line). Parts are ordered added, changed, removed.
fn member_detail(before: &csd_ir::Node, after: &csd_ir::Node) -> Option<String> {
    let delta = member_diff(before, after);
    if delta.added.is_empty() && delta.changed.is_empty() && delta.removed.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    parts.extend(delta.added.iter().map(|m| format!("+{m}")));
    parts.extend(delta.changed.iter().map(|m| format!("~{m}")));
    parts.extend(delta.removed.iter().map(|m| format!("-{m}")));
    Some(format!("({})", parts.join(" ")))
}

/// The lowercase name of a [`NodeKind`] for the textual change list.
fn node_kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Module => "module",
        NodeKind::Struct => "struct",
        NodeKind::Enum => "enum",
        NodeKind::Trait => "trait",
        NodeKind::Fn => "fn",
        NodeKind::Variant => "variant",
        NodeKind::Table => "table",
    }
}

/// The lowercase name of an [`EdgeKind`] for the textual change list.
fn edge_kind_name(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Uses => "uses",
        EdgeKind::Implements => "implements",
        EdgeKind::Associates => "associates",
        EdgeKind::Calls => "calls",
        EdgeKind::Transitions => "transitions",
        EdgeKind::ForeignKey => "foreignkey",
    }
}

/// The last `::`-separated segment of an id: the item's short, human-facing name.
fn short_name(id: &StableId) -> &str {
    id.as_str()
        .rsplit("::")
        .next()
        .unwrap_or_else(|| id.as_str())
}

/// The owning module of an id: everything up to the last `::` segment, or `(crate root)` when the
/// id has no separator.
fn owning_module(id: &StableId) -> String {
    match id.as_str().rsplit_once("::") {
        Some((parent, _)) => parent.to_string(),
        None => "(crate root)".to_string(),
    }
}

/// Node-change buckets for one owning module, in changelog section order.
#[derive(Default)]
struct ChangelogGroup {
    added: Vec<String>,
    removed: Vec<String>,
    modified: Vec<String>,
    moved: Vec<String>,
}

/// Render the delta as a grouped, human-facing Markdown changelog.
///
/// Node changes are grouped by owning module and split into Added, Removed, Modified, and Moved;
/// edge changes are collected into a trailing Dependencies section. This is the textual companion
/// to the rendered diagram: what changed, in prose, for a PR body or release note. Unlike the
/// mechanical `--list`, it is grouped and titled. Pure and deterministic: groups are a `BTreeMap`
/// and within a group the input order (the differ's stable sort) is preserved.
pub fn changelog_markdown(base: &str, changes: &[Change]) -> String {
    use std::collections::BTreeMap;

    let mut groups: BTreeMap<String, ChangelogGroup> = BTreeMap::new();
    // A module change is rendered as a status tag on that module's own section heading, never also
    // as a bullet under its parent, so a changed module is listed exactly once.
    let mut module_status: BTreeMap<String, &'static str> = BTreeMap::new();
    let mut edges: Vec<String> = Vec::new();

    for change in changes {
        match change {
            Change::Added(n) if n.kind == NodeKind::Module => {
                module_status.insert(n.id.as_str().to_string(), "added");
                groups.entry(n.id.as_str().to_string()).or_default();
            }
            Change::Removed(n) if n.kind == NodeKind::Module => {
                module_status.insert(n.id.as_str().to_string(), "removed");
                groups.entry(n.id.as_str().to_string()).or_default();
            }
            Change::Modified { after, .. } if after.kind == NodeKind::Module => {
                module_status.insert(after.id.as_str().to_string(), "modified");
                groups.entry(after.id.as_str().to_string()).or_default();
            }
            Change::Added(n) => {
                groups
                    .entry(owning_module(&n.id))
                    .or_default()
                    .added
                    .push(format!(
                        "{} `{}`",
                        node_kind_name(n.kind),
                        short_name(&n.id)
                    ))
            }
            Change::Removed(n) => groups
                .entry(owning_module(&n.id))
                .or_default()
                .removed
                .push(format!(
                    "{} `{}`",
                    node_kind_name(n.kind),
                    short_name(&n.id)
                )),
            Change::Modified { before, after } => {
                let mut line =
                    format!("{} `{}`", node_kind_name(after.kind), short_name(&after.id));
                if let Some(detail) = member_detail(before, after) {
                    line.push(' ');
                    line.push_str(&detail);
                }
                groups
                    .entry(owning_module(&after.id))
                    .or_default()
                    .modified
                    .push(line);
            }
            Change::Moved { node, from, to } => groups
                .entry(to.as_str().to_string())
                .or_default()
                .moved
                .push(format!("`{}` from `{}`", short_name(node), from.as_str())),
            Change::EdgeAdded(e) => edges.push(format!(
                "+ `{}` -> `{}` ({})",
                e.from.as_str(),
                e.to.as_str(),
                edge_kind_name(e.kind)
            )),
            Change::EdgeRemoved(e) => edges.push(format!(
                "- `{}` -> `{}` ({})",
                e.from.as_str(),
                e.to.as_str(),
                edge_kind_name(e.kind)
            )),
        }
    }

    let mut out = String::new();
    let _ = writeln!(out, "# Structure changelog vs {base}");
    let _ = writeln!(out);
    if groups.is_empty() && edges.is_empty() {
        let _ = writeln!(out, "No structural changes.");
        return out;
    }
    for (module, g) in &groups {
        let status = module_status.get(module.as_str()).copied();
        let empty = g.added.is_empty()
            && g.removed.is_empty()
            && g.modified.is_empty()
            && g.moved.is_empty();
        // A group can be empty when a module changed but none of its contents did; still show its
        // heading so the module change is visible. Skip a genuinely empty, untagged group.
        if empty && status.is_none() {
            continue;
        }
        match status {
            Some(s) => {
                let _ = writeln!(out, "## {module} ({s})");
            }
            None => {
                let _ = writeln!(out, "## {module}");
            }
        }
        let _ = writeln!(out);
        changelog_section(&mut out, "Added", &g.added);
        changelog_section(&mut out, "Removed", &g.removed);
        changelog_section(&mut out, "Modified", &g.modified);
        changelog_section(&mut out, "Moved here", &g.moved);
    }
    if !edges.is_empty() {
        let _ = writeln!(out, "## Dependencies");
        let _ = writeln!(out);
        for e in &edges {
            let _ = writeln!(out, "- {e}");
        }
        let _ = writeln!(out);
    }
    out
}

/// Extract the working tree and print (or write) a per-file structural index. Observational: no
/// gate, no diagram, always exits 0 on success.
fn run_files(out: Option<&str>) -> Result<i32> {
    let repo = repo_root()?;
    let head = extract_head(&repo)?;
    emit_doc(&files_markdown(&head), out)?;
    Ok(0)
}

/// Generate a starter `.csd.toml`, or write it. With no `out`, write `.csd.toml` at the repo root
/// and refuse to overwrite an existing one; `-` prints to stdout; any other path is written.
fn run_init(out: Option<&str>) -> Result<i32> {
    let repo = repo_root()?;
    let head = extract_head(&repo)?;
    let config = init_config(&head);
    match out {
        None => {
            let target = repo.join(".csd.toml");
            if target.exists() {
                bail!(
                    "{} already exists; pass --out <file> to write elsewhere or --out - for stdout",
                    target.display()
                );
            }
            std::fs::write(&target, &config)
                .with_context(|| format!("failed to write {}", target.display()))?;
            println!("wrote {}", target.display());
        }
        Some("-") => print!("{config}"),
        Some(path) => {
            std::fs::write(path, &config).with_context(|| format!("failed to write {path}"))?;
            println!("wrote {path}");
        }
    }
    Ok(0)
}

/// The `crates/<name>/**` (or `src/**`) globs for every crate a module was extracted from, sorted
/// and de-duplicated, so a generated config maps real paths rather than guessed ones.
fn detected_crate_globs(head: &Graph) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut roots: BTreeSet<String> = BTreeSet::new();
    for n in &head.nodes {
        if n.kind != NodeKind::Module {
            continue;
        }
        let file = n.span.file.as_str();
        if let Some(idx) = file.find("/src/") {
            roots.insert(format!("{}/**", &file[..idx]));
        } else if file.starts_with("src/") {
            roots.insert("src/**".to_string());
        }
    }
    roots.into_iter().collect()
}

/// Render a starter `.csd.toml`: a commented layer-mapping template pre-filled with this repo's
/// detected crate globs, plus sensible defaults (module view, cycle lint, new-only ratchet). The
/// layers section is left commented so the config is valid immediately and the user opts in to a
/// constraint by uncommenting and assigning layers.
fn init_config(head: &Graph) -> String {
    let mut s = String::new();
    s.push_str("# cargo-structure-diff configuration, generated by `csd init`.\n");
    s.push_str(
        "# Assign the detected crates to architectural layers, then declare forbidden edges.\n\n",
    );
    s.push_str("[layers]\n");
    s.push_str("# order lists strata top (may depend downward) to bottom, for example:\n");
    s.push_str("# order = [\"app\", \"contract\"]\n");
    s.push_str("# map assigns each module path to a layer (first match wins). Detected crates:\n");
    s.push_str("# map = [\n");
    for glob in detected_crate_globs(head) {
        let _ = writeln!(s, "#     {{ layer = \"app\", glob = \"{glob}\" }},");
    }
    s.push_str("# ]\n");
    s.push_str("# forbid = [{ from = \"contract\", to = \"app\" }]\n\n");
    s.push_str("[views]\nenabled = [\"modules\"]\n\n");
    s.push_str("[lint]\ndeny = [\"cycles\"]\nwarn = [\"fan_in\", \"fan_out\"]\n\n");
    s.push_str("[ratchet]\nmode = \"new-only\"\n");
    s
}

/// The items one source file defines, in section order.
#[derive(Default)]
struct FileItems {
    modules: Vec<String>,
    types: Vec<String>,
    functions: Vec<String>,
    implements: Vec<String>,
}

/// Render a per-file structural index: for each source file, the modules, types (with their
/// members and member types when known), and functions it defines, plus the trait realizations
/// declared in it. Pure and deterministic: files and their nodes come from the normalized graph, so
/// order is stable. This answers "what does this file define" without opening it.
pub fn files_markdown(head: &Graph) -> String {
    use std::collections::BTreeMap;

    let mut by_file: BTreeMap<&str, FileItems> = BTreeMap::new();
    for n in &head.nodes {
        let items = by_file.entry(n.span.file.as_str()).or_default();
        match n.kind {
            NodeKind::Module => items.modules.push(format!("`{}`", short_name(&n.id))),
            NodeKind::Struct | NodeKind::Enum | NodeKind::Trait => items.types.push(type_line(n)),
            NodeKind::Fn => items.functions.push(fn_line(n)),
            // Variants render under their enum's members; tables belong to the schema view.
            NodeKind::Variant | NodeKind::Table => {}
        }
    }

    // Attribute each trait realization to the file its subject node lives in.
    let file_of: BTreeMap<&StableId, &str> = head
        .nodes
        .iter()
        .map(|n| (&n.id, n.span.file.as_str()))
        .collect();
    for e in &head.edges {
        if e.kind == EdgeKind::Implements {
            if let Some(file) = file_of.get(&e.from) {
                by_file.entry(file).or_default().implements.push(format!(
                    "`{}` implements `{}`",
                    short_name(&e.from),
                    short_name(&e.to)
                ));
            }
        }
    }

    let mut out = String::from("# File structure index\n\n");
    out.push_str("Generated by `csd files`: what each source file defines.\n\n");
    for (file, items) in &by_file {
        let _ = writeln!(out, "## `{file}`");
        let _ = writeln!(out);
        changelog_section(&mut out, "Modules", &items.modules);
        changelog_section(&mut out, "Types", &items.types);
        changelog_section(&mut out, "Functions", &items.functions);
        changelog_section(&mut out, "Implements", &items.implements);
    }
    out
}

/// One line for a function node: its name with the captured signature (`foo(a: u32) -> bool`) when
/// the extractor recorded one, else just the name.
fn fn_line(n: &csd_ir::Node) -> String {
    match n.attrs.get("sig") {
        Some(sig) => format!("`{}{sig}`", short_name(&n.id)),
        None => format!("`{}`", short_name(&n.id)),
    }
}

/// One line for a type node: `struct `Name`` plus `{ field: Type, ... }` when member types are
/// known, or a bare member list when only names are, or just the name when it has neither.
fn type_line(n: &csd_ir::Node) -> String {
    let kind = node_kind_name(n.kind);
    let name = short_name(&n.id);
    let Some(fp) = &n.fingerprint else {
        return format!("{kind} `{name}`");
    };
    if !fp.member_types.is_empty() {
        let members: Vec<String> = fp
            .member_types
            .iter()
            .map(|(m, t)| {
                if t.is_empty() {
                    m.clone()
                } else {
                    format!("{m}: {t}")
                }
            })
            .collect();
        format!("{kind} `{name}` {{ {} }}", members.join(", "))
    } else if !fp.members.is_empty() {
        let members: Vec<&str> = fp.members.iter().map(String::as_str).collect();
        format!("{kind} `{name}` {{ {} }}", members.join(", "))
    } else {
        format!("{kind} `{name}`")
    }
}

/// Write one titled bullet section, or nothing when `lines` is empty.
fn changelog_section(out: &mut String, title: &str, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let _ = writeln!(out, "### {title}");
    for l in lines {
        let _ = writeln!(out, "- {l}");
    }
    let _ = writeln!(out);
}

/// Diff the working tree against `base` and print the grouped Markdown changelog. Observational:
/// no gate, no diagram, always exits 0 on success.
fn run_changelog(base: &str) -> Result<i32> {
    let repo = repo_root()?;
    let config = load_config(&repo)?;
    let opts = RenderOpts {
        full: false,
        scope: Vec::new(),
        entry: None,
        format: Format::default(),
    };
    let report = analyze(&repo, base, &config, &opts)?;
    print!("{}", changelog_markdown(base, &report.changes));
    Ok(0)
}

/// Node and edge change tallies, for the drift summary.
#[derive(Default, Debug, PartialEq, Eq)]
struct DriftCounts {
    nodes_added: usize,
    nodes_removed: usize,
    nodes_modified: usize,
    nodes_moved: usize,
    edges_added: usize,
    edges_removed: usize,
}

impl DriftCounts {
    /// The total number of structural changes.
    fn total(&self) -> usize {
        self.nodes_added
            + self.nodes_removed
            + self.nodes_modified
            + self.nodes_moved
            + self.edges_added
            + self.edges_removed
    }
}

/// Tally a delta into [`DriftCounts`].
fn count_changes(changes: &[Change]) -> DriftCounts {
    let mut c = DriftCounts::default();
    for change in changes {
        match change {
            Change::Added(_) => c.nodes_added += 1,
            Change::Removed(_) => c.nodes_removed += 1,
            Change::Modified { .. } => c.nodes_modified += 1,
            Change::Moved { .. } => c.nodes_moved += 1,
            Change::EdgeAdded(_) => c.edges_added += 1,
            Change::EdgeRemoved(_) => c.edges_removed += 1,
        }
    }
    c
}

/// Render a compact drift summary: per-kind counts and a total. `from` labels what head is compared
/// against (a ref or a snapshot path). Pure and deterministic.
fn drift_markdown(from: &str, changes: &[Change]) -> String {
    let c = count_changes(changes);
    let mut out = String::new();
    let _ = writeln!(out, "# Structure drift vs {from}");
    let _ = writeln!(out);
    let _ = writeln!(out, "- nodes added:    {}", c.nodes_added);
    let _ = writeln!(out, "- nodes removed:  {}", c.nodes_removed);
    let _ = writeln!(out, "- nodes modified: {}", c.nodes_modified);
    let _ = writeln!(out, "- nodes moved:    {}", c.nodes_moved);
    let _ = writeln!(out, "- edges added:    {}", c.edges_added);
    let _ = writeln!(out, "- edges removed:  {}", c.edges_removed);
    let _ = writeln!(out);
    let node_total = c.nodes_added + c.nodes_removed + c.nodes_modified + c.nodes_moved;
    let edge_total = c.edges_added + c.edges_removed;
    let _ = writeln!(
        out,
        "Total: {} structural change(s) ({node_total} node, {edge_total} edge).",
        c.total()
    );
    out
}

/// Print a drift summary of the working tree against a base ref or a `--baseline` snapshot.
/// Observational: no gate, always exits 0 on success.
fn run_drift(base: &str, baseline: Option<&str>) -> Result<i32> {
    let repo = repo_root()?;
    let config = load_config(&repo)?;
    let opts = RenderOpts {
        full: false,
        scope: Vec::new(),
        entry: None,
        format: Format::default(),
    };
    let (report, label) = match baseline {
        Some(path) => {
            let graph = load_baseline(path)?;
            (analyze_baseline(&repo, &graph, &config, &opts)?, path)
        }
        None => (analyze(&repo, base, &config, &opts)?, base),
    };
    print!("{}", drift_markdown(label, &report.changes));
    Ok(0)
}

/// The [`View`] a name selects, or `None` for an unknown name.
fn view_from_name(name: &str) -> Option<View> {
    Some(match name {
        "modules" => View::Modules,
        "states" => View::States,
        "types" => View::Types,
        "calls" => View::Calls,
        "callgraph" => View::CallGraph,
        "schema" => View::Schema,
        "overview" => View::Overview,
        _ => return None,
    })
}

/// The first-parent commit shas in `base..head`, oldest to newest.
fn commits_first_parent(repo: &Path, base: &str, head: &str) -> Result<Vec<String>> {
    let range = format!("{base}..{head}");
    let out = git(repo, &["rev-list", "--first-parent", "--reverse", &range])
        .with_context(|| format!("failed to list commits in {range}"))?;
    Ok(out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// The subject line of a commit.
fn git_subject(repo: &Path, sha: &str) -> Result<String> {
    Ok(git(repo, &["show", "-s", "--format=%s", sha])?
        .trim()
        .to_string())
}

/// Render one frame per first-parent commit in `base..head`, oldest to newest.
///
/// Each commit is materialized in a throwaway worktree and extracted. By default a frame is the
/// delta from its parent (the previous commit, or `base` for the first); with `full`, each frame is
/// the whole graph at that commit. Deterministic: the commit order and the per-commit extraction are
/// both stable. Observational, always exits 0 on success.
fn run_walk(base: &str, head: &str, view_name: &str, full: bool, format: Format) -> Result<i32> {
    let repo = repo_root()?;
    let config = load_config(&repo)?;
    let view = view_from_name(view_name).with_context(|| {
        format!("unknown --view {view_name:?}; expected modules, states, types, calls, callgraph, schema, or overview")
    })?;

    let shas = commits_first_parent(&repo, base, head)?;
    if shas.is_empty() {
        println!("no first-parent commits in {base}..{head}");
        return Ok(0);
    }

    // The parent of the first frame is `base` itself, so a delta frame has a prior graph to diff.
    let mut prev = base_graph(&repo, base)?;
    for sha in &shas {
        let graph = base_graph(&repo, sha)?;
        let subject = git_subject(&repo, sha)?;
        let short = sha.get(..7).unwrap_or(sha.as_str());
        let changes = if full {
            Vec::new()
        } else {
            diff(
                &prev,
                &graph,
                DiffOptions {
                    rename_threshold: Some(0.7),
                    file_renames: Vec::new(),
                },
            )
        };
        let opts = RenderOpts {
            full,
            scope: Vec::new(),
            entry: Some(call_entry(&graph, &config)),
            format,
        };
        println!("=== {short} {subject} ===");
        println!("{}", render(view, &graph, &changes, &opts));
        println!();
        prev = graph;
    }
    Ok(0)
}

/// Default views for `csd doc` when `views.enabled` is empty: every view csd knows.
const DOC_DEFAULT_VIEWS: &[&str] = &[
    "overview",
    "modules",
    "types",
    "states",
    "calls",
    "callgraph",
    "schema",
];

/// Emit the whole-codebase structure report (a snapshot of head, not a delta).
///
/// Extracts the current working tree once, renders each enabled view as a full snapshot (empty
/// delta, `full = true`, so the whole graph is drawn as context with no delta colour), and writes a
/// single self-contained Markdown document to `out` or stdout. Observational: exits 0 on success.
fn run_doc(out: Option<&str>) -> Result<i32> {
    let repo = repo_root()?;
    let config = load_config(&repo)?;
    let head = extract_head(&repo)?;
    let doc = doc_markdown(&head, &config);
    emit_doc(&doc, out)?;
    Ok(0)
}

/// Write the rendered report to `out`, or stdout when `out` is `None`.
fn emit_doc(doc: &str, out: Option<&str>) -> Result<()> {
    match out {
        Some(path) => std::fs::write(path, doc).with_context(|| format!("failed to write {path}")),
        None => {
            print!("{doc}");
            Ok(())
        }
    }
}

/// Render the head graph as a self-contained Markdown structure report: a title, a generated-by
/// note, a module index, then one section per enabled view with the diagram in a ```mermaid fenced
/// block (GitHub and VS Code render these). An HTML variant is a future option (SchemaSpy-style).
fn doc_markdown(head: &Graph, config: &Config) -> String {
    // Snapshot options: draw the whole graph as context, with no delta colour, as Mermaid.
    let snapshot = |view: View| -> String {
        render(
            view,
            head,
            &[],
            &RenderOpts {
                full: true,
                scope: Vec::new(),
                entry: Some(call_entry(head, config)),
                format: Format::Mermaid,
            },
        )
    };

    let enabled: Vec<String> = if config.views.enabled.is_empty() {
        DOC_DEFAULT_VIEWS.iter().map(|s| s.to_string()).collect()
    } else {
        config.views.enabled.clone()
    };

    let mut out = String::from("# Structure report\n\n");
    out.push_str(
        "Generated by cargo-structure-diff (`csd doc`): a snapshot of the current working tree.\n\n",
    );

    out.push_str("## Module index\n\n");
    let modules: Vec<&str> = head
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Module)
        .map(|n| n.id.as_str())
        .collect();
    if modules.is_empty() {
        out.push_str("_No modules found._\n\n");
    } else {
        for id in modules {
            let _ = writeln!(out, "- `{id}`");
        }
        out.push('\n');
    }

    for name in enabled {
        let view = match name.as_str() {
            "modules" => View::Modules,
            "states" => View::States,
            "types" => View::Types,
            "calls" => View::Calls,
            "callgraph" => View::CallGraph,
            "schema" => View::Schema,
            "overview" => View::Overview,
            _ => continue,
        };
        let _ = writeln!(out, "## {name} view\n");
        out.push_str("```mermaid\n");
        out.push_str(&snapshot(view));
        out.push_str("\n```\n\n");
    }

    out
}

/// Run `cmd` in the base worktree and in the head working tree, extract the emitted
/// `sequenceDiagram` Mermaid blocks, and print the observed-trace delta.
///
/// Traced mode is opt-in and observational: it exits 0 whatever the delta, and only 2 when the
/// command itself fails to run. The base worktree is always torn down, even on error.
fn run_trace(base: &str, cmd: &str) -> Result<i32> {
    let repo = repo_root()?;
    let (base_blocks, head_blocks) = observe_traces(&repo, base, cmd)?;
    let (added, removed) = trace_delta(&base_blocks, &head_blocks);

    println!(
        "cargo-structure-diff trace (observational): {} base trace(s), {} head trace(s), {} added, {} removed vs {base}",
        base_blocks.len(),
        head_blocks.len(),
        added.len(),
        removed.len(),
    );
    for block in &added {
        println!("\n+ added trace:\n{block}");
    }
    for block in &removed {
        println!("\n- removed trace:\n{block}");
    }
    Ok(0)
}

/// Run `cmd` in the base worktree and in `repo` (head), returning the normalized sequence blocks
/// observed on each side. The base worktree is always cleaned up. Testable without touching cwd.
fn observe_traces(repo: &Path, base: &str, cmd: &str) -> Result<(Vec<String>, Vec<String>)> {
    let base_out = trace_base(repo, base, cmd)?;
    let head_out = run_shell(repo, cmd)
        .with_context(|| format!("failed to run trace command {cmd:?} in head"))?;
    Ok((
        extract_mermaid_sequences(&base_out),
        extract_mermaid_sequences(&head_out),
    ))
}

/// Run `cmd` in a throwaway detached worktree at `base`, always removing the worktree afterward.
fn trace_base(repo: &Path, base: &str, cmd: &str) -> Result<String> {
    let worktree = worktree_path(base);
    git(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            &worktree.to_string_lossy(),
            base,
        ],
    )
    .with_context(|| format!("failed to add base worktree for {base:?}"))?;
    let result = run_shell(&worktree, cmd)
        .with_context(|| format!("failed to run trace command {cmd:?} in {base:?}"));
    cleanup(repo, &worktree);
    result
}

/// Run a shell command via `sh -c <cmd>` in `dir` and return its stdout. Errors only if the
/// process cannot be spawned; a non-zero exit still yields whatever was printed.
fn run_shell(dir: &Path, cmd: &str) -> Result<String> {
    let out = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(dir)
        .output()
        .with_context(|| format!("failed to spawn shell for {cmd:?} in {}", dir.display()))?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Extract fenced ```mermaid blocks whose body is a `sequenceDiagram`, normalized (each line and
/// the block trailing-trimmed). Non-mermaid fences and other Mermaid kinds (flowchart, etc.) are
/// ignored, and set semantics are the caller's job.
pub fn extract_mermaid_sequences(stdout: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut lines = stdout.lines();
    while let Some(line) = lines.next() {
        let info = line.trim_start();
        if info.strip_prefix("```").map(str::trim) != Some("mermaid") {
            continue;
        }
        let mut body = Vec::new();
        for inner in lines.by_ref() {
            if inner.trim_start().starts_with("```") {
                break;
            }
            body.push(inner.trim_end());
        }
        // First non-empty body line decides the diagram kind.
        let is_sequence = body
            .iter()
            .find(|l| !l.trim().is_empty())
            .is_some_and(|l| l.trim_start().starts_with("sequenceDiagram"));
        if is_sequence {
            blocks.push(body.join("\n").trim_end().to_string());
        }
    }
    blocks
}

/// Set diff of normalized trace blocks: those in head but not base (added) and in base but not head
/// (removed). Order follows each input; duplicates within a side are preserved by that side's list.
pub fn trace_delta(base_blocks: &[String], head_blocks: &[String]) -> (Vec<String>, Vec<String>) {
    let added = head_blocks
        .iter()
        .filter(|b| !base_blocks.contains(b))
        .cloned()
        .collect();
    let removed = base_blocks
        .iter()
        .filter(|b| !head_blocks.contains(b))
        .cloned()
        .collect();
    (added, removed)
}

/// The repo root of the current working directory (`git rev-parse --show-toplevel`).
fn repo_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let out = git(&cwd, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(out.trim()))
}

/// Load `.csd.toml` from `repo` if present, else the default config.
fn load_config(repo: &Path) -> Result<Config> {
    let path = repo.join(".csd.toml");
    if path.exists() {
        Ok(Config::load(&path)?)
    } else {
        Ok(Config::default())
    }
}

/// Run the whole pipeline against `repo`: base ref vs the current working tree.
///
/// The base ref is materialized in a throwaway detached worktree that is always removed, even on
/// error. Head is the working tree as-is. This is testable without changing the process cwd.
pub fn analyze(repo: &Path, base: &str, config: &Config, opts: &RenderOpts) -> Result<Report> {
    let base_graph = base_graph(repo, base)?;
    let head_graph = extract_head(repo)?;
    let file_renames = file_renames(repo, base)?;
    Ok(analyze_graphs(
        base_graph,
        head_graph,
        file_renames,
        config,
        opts,
    ))
}

/// Diff the working tree against a pinned baseline graph loaded from a snapshot file.
///
/// Same pipeline as [`analyze`], but the base side comes from a serialized [`Graph`] rather than a
/// git worktree, so there is no ref to derive git file-renames from (module reconciliation from git
/// renames is therefore skipped; the structural fingerprint matcher still runs).
pub fn analyze_baseline(
    repo: &Path,
    baseline: &Graph,
    config: &Config,
    opts: &RenderOpts,
) -> Result<Report> {
    let head_graph = extract_head(repo)?;
    Ok(analyze_graphs(
        baseline.clone(),
        head_graph,
        Vec::new(),
        config,
        opts,
    ))
}

/// The shared diff/lint/render core, parameterized on the two graphs and any known file renames.
fn analyze_graphs(
    base_graph: Graph,
    head_graph: Graph,
    file_renames: Vec<FileRename>,
    config: &Config,
    opts: &RenderOpts,
) -> Report {
    let changes = diff(
        &base_graph,
        &head_graph,
        DiffOptions {
            rename_threshold: Some(0.7),
            file_renames,
        },
    );
    let findings = lint(&head_graph, &changes, config);
    let diagrams = render_views(&head_graph, &changes, config, opts);
    Report {
        changes,
        findings,
        diagrams,
    }
}

/// Extract the working tree and serialize it as a pretty-printed JSON snapshot.
///
/// The snapshot is a pinned baseline: a later `csd diff --baseline <file>` diffs a new working tree
/// against it without needing the original ref checked out. Deterministic, since the graph is
/// normalized before extraction returns.
fn snapshot_json(repo: &Path) -> Result<String> {
    let head = extract_head(repo)?;
    serde_json::to_string_pretty(&head).context("failed to serialize the structure snapshot")
}

/// Load a baseline graph from a JSON snapshot file produced by `csd snapshot`.
fn load_baseline(path: &str) -> Result<Graph> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("failed to read baseline {path}"))?;
    serde_json::from_str(&text).with_context(|| format!("failed to parse baseline {path}"))
}

/// Extract the current working tree as one head graph: the Rust module graph merged with the
/// schema graph, so the schema view and schema lints see the same head both `diff` and `doc` use.
fn extract_head(repo: &Path) -> Result<Graph> {
    let mut head = csd_extract_rs::extract(repo).context("failed to extract head working tree")?;
    let schema = csd_extract_db::extract_schema(repo).context("failed to extract head schema")?;
    merge_schema(&mut head, schema);
    Ok(head)
}

/// Merge a schema-view graph's `Table` nodes and `ForeignKey` edges into `graph`, then renormalize
/// so ordering stays byte-stable. A schema-less repo yields an empty graph, so this is a no-op.
fn merge_schema(graph: &mut Graph, schema: Graph) {
    graph.nodes.extend(schema.nodes);
    graph.edges.extend(schema.edges);
    graph.normalize();
}

/// Render one diagram per enabled view. Defaults to the module view when `views.enabled` is empty;
/// unknown view names are ignored. The state lints run via [`lint`] independent of this.
fn render_views(
    head: &Graph,
    changes: &[Change],
    config: &Config,
    opts: &RenderOpts,
) -> Vec<ViewDiagram> {
    let enabled: Vec<String> = if config.views.enabled.is_empty() {
        vec!["modules".to_string()]
    } else {
        config.views.enabled.clone()
    };
    let mut diagrams = Vec::new();
    for name in enabled {
        let view = match name.as_str() {
            "modules" => View::Modules,
            "states" => View::States,
            "types" => View::Types,
            "calls" => View::Calls,
            "callgraph" => View::CallGraph,
            "schema" => View::Schema,
            "overview" => View::Overview,
            _ => continue,
        };
        // Thread the CLI opts through; the entry (calls view only) always comes from config.
        let view_opts = RenderOpts {
            full: opts.full,
            scope: opts.scope.clone(),
            entry: Some(call_entry(head, config)),
            format: opts.format,
        };
        let mermaid = render(view, head, changes, &view_opts);
        diagrams.push(ViewDiagram {
            view: name,
            mermaid,
        });
    }
    diagrams
}

/// The entry function for the calls sequence slice: the configured `views.entry` when set,
/// otherwise the lowest-id function that has an outgoing call (deterministic). Falls back to an
/// empty id, which the renderer reports as no calls.
fn call_entry(head: &Graph, config: &Config) -> StableId {
    if let Some(entry) = &config.views.entry {
        return StableId::new(entry.clone());
    }
    head.edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls)
        .map(|e| &e.from)
        .min()
        .cloned()
        .unwrap_or_else(|| StableId::new(""))
}

/// Extract the base ref by checking it out into a throwaway detached worktree.
///
/// The worktree is always torn down before returning, so a failed extraction never leaks it (the
/// guaranteed-cleanup pattern from the harness).
fn base_graph(repo: &Path, base: &str) -> Result<Graph> {
    let worktree = worktree_path(base);
    git(
        repo,
        &[
            "worktree",
            "add",
            "--detach",
            &worktree.to_string_lossy(),
            base,
        ],
    )
    .with_context(|| format!("failed to add base worktree for {base:?}"))?;
    // Extract the module graph and the schema graph from the one base worktree, then merge, so the
    // schema view and the destructive_migration lint see both sides of the delta.
    let result = (|| {
        let mut graph = csd_extract_rs::extract(&worktree)
            .with_context(|| format!("failed to extract {base:?}"))?;
        let schema = csd_extract_db::extract_schema(&worktree)
            .with_context(|| format!("failed to extract {base:?} schema"))?;
        merge_schema(&mut graph, schema);
        Ok(graph)
    })();
    cleanup(repo, &worktree);
    result
}

/// Git file renames from `base` to the working tree (`git diff -M50 --name-status <base>`).
fn file_renames(repo: &Path, base: &str) -> Result<Vec<FileRename>> {
    let out = git(repo, &["diff", "-M50", "--name-status", base])?;
    let mut renames = Vec::new();
    for line in out.lines() {
        // A rename line is `R<score>\t<old>\t<new>`; other statuses (A/M/D) are ignored.
        let mut parts = line.split('\t');
        if !parts.next().unwrap_or("").starts_with('R') {
            continue;
        }
        let (Some(old_path), Some(new_path)) = (parts.next(), parts.next()) else {
            continue;
        };
        renames.push(FileRename {
            old_path: old_path.to_string(),
            new_path: new_path.to_string(),
        });
    }
    Ok(renames)
}

/// Remove the worktree from git's registry and delete its directory. Best-effort: runs on the
/// error path too, so failures here must not mask the original error and are ignored.
fn cleanup(repo: &Path, worktree: &Path) {
    let _ = git(
        repo,
        &["worktree", "remove", "--force", &worktree.to_string_lossy()],
    );
    let _ = std::fs::remove_dir_all(worktree);
}

/// A unique worktree path under the system temp dir. Keyed by PID, a process-wide sequence, and a
/// sanitized ref so concurrent runs and parallel tests never collide, and it lives outside the repo.
fn worktree_path(base: &str) -> PathBuf {
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let safe: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    std::env::temp_dir().join(format!("csd-wt-{}-{seq}-{safe}", std::process::id()))
}

/// Run git in `dir` and return its stdout, erroring on a non-zero exit.
fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| format!("failed to spawn git in {}", dir.display()))?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    String::from_utf8(out.stdout).context("git output was not utf-8")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp dir removed on drop, so worktrees and fixtures never leak past a test.
    struct TmpRepo {
        path: PathBuf,
    }

    impl TmpRepo {
        fn new(tag: &str) -> Self {
            let seq = worktree_path(tag); // reuse the unique-name scheme for a unique dir
            let path = std::env::temp_dir().join(format!(
                "csd-cli-test-{}-{}",
                std::process::id(),
                seq.file_name().unwrap().to_string_lossy()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self { path }
        }
    }

    impl Drop for TmpRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Run git in `dir` with an inline throwaway identity and assert success.
    fn git_ok(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "user.name=test",
            ])
            .args(args)
            .status()
            .expect("failed to spawn git");
        assert!(status.success(), "git {} failed", args.join(" "));
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    const CONFIG: &str = "\
[layers]
order = [\"app\", \"infra\"]
map = [
    { layer = \"app\", glob = \"src/app/**\" },
    { layer = \"infra\", glob = \"src/infra/**\" },
]
forbid = [{ from = \"app\", to = \"infra\" }]

[lint]
deny = [\"layering\", \"cycles\"]
";

    /// Build a repo committed to `main` with two layers and no forbidden edge.
    fn base_repo(tag: &str) -> TmpRepo {
        let repo = TmpRepo::new(tag);
        let dir = &repo.path;
        git_ok(dir, &["-c", "init.defaultBranch=main", "init", "-q"]);
        write(dir, ".csd.toml", CONFIG);
        write(dir, "src/lib.rs", "pub mod app;\npub mod infra;\n");
        write(dir, "src/app/mod.rs", "pub fn app_fn() {}\n");
        write(dir, "src/infra/mod.rs", "pub fn infra_fn() {}\n");
        git_ok(dir, &["add", "-A"]);
        git_ok(dir, &["commit", "-q", "-m", "base"]);
        repo
    }

    #[test]
    fn forbidden_edge_in_head_denies_and_exits_one() {
        let repo = base_repo("deny");
        let dir = &repo.path;
        // Head working tree adds a forbidden app -> infra module edge.
        write(
            dir,
            "src/app/mod.rs",
            "use crate::infra;\npub fn app_fn() {}\n",
        );

        let config = Config::load(&dir.join(".csd.toml")).unwrap();
        let report = analyze(dir, "main", &config, &RenderOpts::default()).unwrap();

        assert_eq!(report.exit_code(), 1, "a denied finding must exit 1");
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.rule == "layering" && f.message.contains("must not depend on")),
            "expected a layering finding, got {:?}",
            report.findings
        );
        let module = report
            .diagrams
            .iter()
            .find(|d| d.view == "modules")
            .expect("a module view is rendered by default");
        assert!(
            module.mermaid.contains("flowchart"),
            "diagram must be a Mermaid flowchart:\n{}",
            module.mermaid
        );

        // No worktree leaked past analyze.
        let list = git(dir, &["worktree", "list"]).unwrap();
        assert_eq!(
            list.lines().count(),
            1,
            "only main worktree remains:\n{list}"
        );
    }

    #[test]
    fn no_violation_in_head_exits_zero() {
        let repo = base_repo("allow");
        let dir = &repo.path;
        // Head adds a benign item, no forbidden edge.
        write(
            dir,
            "src/app/mod.rs",
            "pub fn app_fn() {}\npub struct Extra;\n",
        );

        let config = Config::load(&dir.join(".csd.toml")).unwrap();
        let report = analyze(dir, "main", &config, &RenderOpts::default()).unwrap();

        assert_eq!(report.exit_code(), 0, "no denial must exit 0");
        assert!(
            !report.denied(),
            "no denied findings expected, got {:?}",
            report.findings
        );
    }

    #[test]
    fn preexisting_violation_stays_green_under_new_only_ratchet() {
        // Base already contains the forbidden app -> infra edge, committed to main.
        let repo = TmpRepo::new("ratchet");
        let dir = &repo.path;
        git_ok(dir, &["-c", "init.defaultBranch=main", "init", "-q"]);
        write(dir, ".csd.toml", CONFIG);
        write(dir, "src/lib.rs", "pub mod app;\npub mod infra;\n");
        write(
            dir,
            "src/app/mod.rs",
            "use crate::infra;\npub fn app_fn() {}\n",
        );
        write(dir, "src/infra/mod.rs", "pub fn infra_fn() {}\n");
        git_ok(dir, &["add", "-A"]);
        git_ok(
            dir,
            &["commit", "-q", "-m", "base with a pre-existing violation"],
        );

        // Head leaves the forbidden edge untouched: the violation is not new.
        let config = Config::load(&dir.join(".csd.toml")).unwrap();
        let report = analyze(dir, "main", &config, &RenderOpts::default()).unwrap();

        assert_eq!(
            report.exit_code(),
            0,
            "a pre-existing violation must not fail the default new-only ratchet, findings: {:?}",
            report.findings
        );
    }

    #[test]
    fn drift_counts_and_summarizes_a_delta() {
        let changes = vec![
            Change::Added(cl_node("crate::A", NodeKind::Struct)),
            Change::Added(cl_node("crate::B", NodeKind::Struct)),
            Change::Removed(cl_node("crate::C", NodeKind::Fn)),
            Change::Moved {
                node: StableId::new("crate::D"),
                from: StableId::new("crate::x"),
                to: StableId::new("crate::y"),
            },
            Change::EdgeAdded(csd_ir::Edge {
                from: StableId::new("crate::A"),
                to: StableId::new("crate::B"),
                kind: EdgeKind::Uses,
                span: cl_node("s", NodeKind::Module).span,
                ordinal: None,
            }),
        ];
        let c = count_changes(&changes);
        assert_eq!(c.nodes_added, 2);
        assert_eq!(c.nodes_removed, 1);
        assert_eq!(c.nodes_moved, 1);
        assert_eq!(c.edges_added, 1);
        assert_eq!(c.total(), 5);

        let md = drift_markdown("v1.0", &changes);
        assert!(md.starts_with("# Structure drift vs v1.0\n"));
        assert!(md.contains("- nodes added:    2"));
        assert!(md.contains("Total: 5 structural change(s) (4 node, 1 edge)."));
    }

    #[test]
    fn view_from_name_maps_known_and_rejects_unknown() {
        assert_eq!(view_from_name("modules"), Some(View::Modules));
        assert_eq!(view_from_name("overview"), Some(View::Overview));
        assert_eq!(view_from_name("bogus"), None);
    }

    #[test]
    fn walk_parses_range_and_view_options() {
        let cmd = parse_args(&[
            "walk".into(),
            "--base".into(),
            "v1".into(),
            "--head".into(),
            "v2".into(),
            "--view".into(),
            "overview".into(),
            "--full".into(),
            "--format".into(),
            "ascii".into(),
        ])
        .unwrap();
        assert_eq!(
            cmd,
            Cmd::Walk {
                base: "v1".into(),
                head: "v2".into(),
                view: "overview".into(),
                full: true,
                format: Format::Ascii,
            }
        );
    }

    #[test]
    fn walk_defaults_head_to_working_head_and_view_to_modules() {
        let cmd = parse_args(&["walk".into()]).unwrap();
        assert_eq!(
            cmd,
            Cmd::Walk {
                base: "main".into(),
                head: "HEAD".into(),
                view: "modules".into(),
                full: false,
                format: Format::Mermaid,
            }
        );
    }

    #[test]
    fn commits_first_parent_lists_oldest_to_newest() {
        let repo = TmpRepo::new("walk");
        let dir = &repo.path;
        git_ok(dir, &["-c", "init.defaultBranch=main", "init", "-q"]);
        write(dir, "src/lib.rs", "pub struct A;\n");
        git_ok(dir, &["add", "-A"]);
        git_ok(dir, &["commit", "-q", "-m", "c1"]);
        let base = git(dir, &["rev-parse", "HEAD"]).unwrap().trim().to_string();
        write(dir, "src/lib.rs", "pub struct A;\npub struct B;\n");
        git_ok(dir, &["add", "-A"]);
        git_ok(dir, &["commit", "-q", "-m", "c2"]);
        write(
            dir,
            "src/lib.rs",
            "pub struct A;\npub struct B;\npub struct C;\n",
        );
        git_ok(dir, &["add", "-A"]);
        git_ok(dir, &["commit", "-q", "-m", "c3"]);

        let shas = commits_first_parent(dir, &base, "HEAD").unwrap();
        assert_eq!(
            shas.len(),
            2,
            "base..HEAD excludes base, includes c2 and c3"
        );
        let subjects: Vec<String> = shas.iter().map(|s| git_subject(dir, s).unwrap()).collect();
        assert_eq!(subjects, ["c2", "c3"], "oldest to newest");
    }

    #[test]
    fn parse_baseline_sets_the_snapshot_path() {
        let cmd =
            parse_args(&["diff".into(), "--baseline".into(), "structure.json".into()]).unwrap();
        match cmd {
            Cmd::Diff { baseline, .. } => {
                assert_eq!(baseline.as_deref(), Some("structure.json"))
            }
            other => panic!("expected Diff, got {other:?}"),
        }
    }

    #[test]
    fn parse_snapshot_subcommand() {
        assert_eq!(
            parse_args(&["snapshot".into()]).unwrap(),
            Cmd::Snapshot { out: None }
        );
        assert_eq!(
            parse_args(&["snapshot".into(), "-o".into(), "s.json".into()]).unwrap(),
            Cmd::Snapshot {
                out: Some("s.json".into())
            }
        );
    }

    #[test]
    fn pinned_snapshot_gates_like_a_git_ref() {
        let repo = base_repo("snap-gate");
        let dir = &repo.path;
        let config = Config::load(&dir.join(".csd.toml")).unwrap();

        // Pin the current structure to a JSON snapshot file, then load it back.
        let snap = dir.join("structure.json");
        std::fs::write(&snap, snapshot_json(dir).unwrap()).unwrap();
        let baseline = load_baseline(&snap.to_string_lossy()).unwrap();

        // The unchanged working tree has no delta against its own snapshot.
        let clean = analyze_baseline(dir, &baseline, &config, &RenderOpts::default()).unwrap();
        assert_eq!(clean.exit_code(), 0);
        assert!(
            clean.changes.is_empty(),
            "unchanged tree must have an empty delta, got {:?}",
            clean.changes
        );

        // A new forbidden edge in head denies against the pinned baseline, exactly as against a ref.
        write(
            dir,
            "src/app/mod.rs",
            "use crate::infra;\npub fn app_fn() {}\n",
        );
        let report = analyze_baseline(dir, &baseline, &config, &RenderOpts::default()).unwrap();
        assert_eq!(
            report.exit_code(),
            1,
            "a new forbidden edge must deny against the snapshot"
        );
        assert!(
            report.findings.iter().any(|f| f.rule == "layering"),
            "expected a layering finding, got {:?}",
            report.findings
        );
    }

    /// A single-crate repo whose config enables the states view and denies new state cycles, with a
    /// base state machine that is acyclic (Red -> Green -> Yellow, Yellow self-loop dropped).
    fn state_repo(tag: &str) -> TmpRepo {
        let repo = TmpRepo::new(tag);
        let dir = &repo.path;
        git_ok(dir, &["-c", "init.defaultBranch=main", "init", "-q"]);
        write(
            dir,
            ".csd.toml",
            "[views]\nenabled = [\"modules\", \"states\"]\n\n[lint]\ndeny = [\"new_state_cycle\"]\n",
        );
        write(
            dir,
            "src/lib.rs",
            "pub enum Light { Red, Green, Yellow }\n\
             impl Light {\n\
             \x20   pub fn step(self) -> Self {\n\
             \x20       match self {\n\
             \x20           Light::Red => Light::Green,\n\
             \x20           Light::Green => Light::Yellow,\n\
             \x20           Light::Yellow => Light::Yellow,\n\
             \x20       }\n\
             \x20   }\n\
             }\n",
        );
        git_ok(dir, &["add", "-A"]);
        git_ok(dir, &["commit", "-q", "-m", "base"]);
        repo
    }

    #[test]
    fn new_state_cycle_denies_and_renders_state_diagram() {
        let repo = state_repo("states");
        let dir = &repo.path;
        // Head closes the loop: Yellow -> Red creates the cycle Red -> Green -> Yellow -> Red.
        write(
            dir,
            "src/lib.rs",
            "pub enum Light { Red, Green, Yellow }\n\
             impl Light {\n\
             \x20   pub fn step(self) -> Self {\n\
             \x20       match self {\n\
             \x20           Light::Red => Light::Green,\n\
             \x20           Light::Green => Light::Yellow,\n\
             \x20           Light::Yellow => Light::Red,\n\
             \x20       }\n\
             \x20   }\n\
             }\n",
        );

        let config = Config::load(&dir.join(".csd.toml")).unwrap();
        let report = analyze(dir, "main", &config, &RenderOpts::default()).unwrap();

        assert_eq!(report.exit_code(), 1, "a new state cycle must exit 1");
        assert!(
            report.findings.iter().any(|f| f.rule == "new_state_cycle"),
            "expected a new_state_cycle finding, got {:?}",
            report.findings
        );
        let state = report
            .diagrams
            .iter()
            .find(|d| d.view == "states")
            .expect("the states view is enabled");
        assert!(
            state.mermaid.contains("stateDiagram-v2"),
            "state diagram expected:\n{}",
            state.mermaid
        );
    }

    #[test]
    fn types_view_renders_a_class_diagram() {
        let repo = TmpRepo::new("types");
        let dir = &repo.path;
        git_ok(dir, &["-c", "init.defaultBranch=main", "init", "-q"]);
        write(dir, ".csd.toml", "[views]\nenabled = [\"types\"]\n");
        write(dir, "src/lib.rs", "pub struct A;\n");
        git_ok(dir, &["add", "-A"]);
        git_ok(dir, &["commit", "-q", "-m", "base"]);
        // Head adds a type, so the type view has a changed class to draw.
        write(dir, "src/lib.rs", "pub struct A;\npub struct B;\n");

        let config = Config::load(&dir.join(".csd.toml")).unwrap();
        let report = analyze(dir, "main", &config, &RenderOpts::default()).unwrap();

        assert_eq!(report.exit_code(), 0, "no lints denied, so exit 0");
        let types = report
            .diagrams
            .iter()
            .find(|d| d.view == "types")
            .expect("the types view is enabled");
        assert!(
            types.mermaid.contains("classDiagram"),
            "class diagram expected:\n{}",
            types.mermaid
        );
        assert!(
            types.mermaid.contains("crate::B"),
            "the added type should appear:\n{}",
            types.mermaid
        );
    }

    #[test]
    fn calls_view_renders_a_sequence_diagram() {
        let repo = TmpRepo::new("calls");
        let dir = &repo.path;
        git_ok(dir, &["-c", "init.defaultBranch=main", "init", "-q"]);
        write(
            dir,
            ".csd.toml",
            "[views]\nenabled = [\"calls\"]\nentry = \"crate::entry\"\n",
        );
        write(
            dir,
            "src/lib.rs",
            "pub fn helper() {}\npub fn entry() { helper(); }\n",
        );
        git_ok(dir, &["add", "-A"]);
        git_ok(dir, &["commit", "-q", "-m", "base"]);
        // Head adds a second call from the entry point.
        write(
            dir,
            "src/lib.rs",
            "pub fn helper() {}\npub fn helper2() {}\npub fn entry() { helper(); helper2(); }\n",
        );

        let config = Config::load(&dir.join(".csd.toml")).unwrap();
        let report = analyze(dir, "main", &config, &RenderOpts::default()).unwrap();

        assert_eq!(report.exit_code(), 0, "no lints denied, so exit 0");
        let calls = report
            .diagrams
            .iter()
            .find(|d| d.view == "calls")
            .expect("the calls view is enabled");
        assert!(
            calls.mermaid.contains("sequenceDiagram"),
            "sequence diagram expected:\n{}",
            calls.mermaid
        );
        assert!(
            calls.mermaid.contains("helper2"),
            "the newly called fn should appear:\n{}",
            calls.mermaid
        );
    }

    /// A repo enabling every view, with a head that adds a module so the DAG views have content.
    fn all_views_repo(tag: &str) -> TmpRepo {
        let repo = TmpRepo::new(tag);
        let dir = &repo.path;
        git_ok(dir, &["-c", "init.defaultBranch=main", "init", "-q"]);
        write(
            dir,
            ".csd.toml",
            "[views]\nenabled = [\"modules\", \"states\", \"types\", \"calls\", \"callgraph\", \"schema\"]\n",
        );
        write(dir, "src/lib.rs", "pub mod a;\n");
        write(dir, "src/a.rs", "pub fn helper() {}\n");
        git_ok(dir, &["add", "-A"]);
        git_ok(dir, &["commit", "-q", "-m", "base"]);
        // Head adds a second module, so the module/call-graph views have an added node to draw.
        write(dir, "src/lib.rs", "pub mod a;\npub mod b;\n");
        write(dir, "src/b.rs", "pub fn other() {}\n");
        repo
    }

    #[test]
    fn dot_format_emits_a_digraph() {
        let repo = all_views_repo("dot");
        let dir = &repo.path;
        let config = Config::load(&dir.join(".csd.toml")).unwrap();
        let opts = RenderOpts {
            format: Format::Dot,
            ..RenderOpts::default()
        };
        let report = analyze(dir, "main", &config, &opts).unwrap();

        let modules = report
            .diagrams
            .iter()
            .find(|d| d.view == "modules")
            .expect("the modules view is enabled");
        assert!(
            modules.mermaid.contains("digraph {"),
            "dot output must be a digraph:\n{}",
            modules.mermaid
        );
        assert!(
            !modules.mermaid.contains("flowchart"),
            "dot output must not be Mermaid:\n{}",
            modules.mermaid
        );
    }

    #[test]
    fn ascii_format_emits_tree_text() {
        let repo = all_views_repo("ascii");
        let dir = &repo.path;
        let config = Config::load(&dir.join(".csd.toml")).unwrap();
        let opts = RenderOpts {
            format: Format::Ascii,
            ..RenderOpts::default()
        };
        let report = analyze(dir, "main", &config, &opts).unwrap();

        let modules = report
            .diagrams
            .iter()
            .find(|d| d.view == "modules")
            .expect("the modules view is enabled");
        // The added module is drawn as a `+`-marked tree node, not a Mermaid or DOT header.
        assert!(
            modules.mermaid.contains('+') && !modules.mermaid.contains("no structural changes"),
            "ascii tree with the added module expected:\n{}",
            modules.mermaid
        );
        assert!(
            !modules.mermaid.contains("flowchart") && !modules.mermaid.contains("digraph"),
            "ascii output must be neither Mermaid nor DOT:\n{}",
            modules.mermaid
        );
    }

    #[test]
    fn parse_reads_full_scope_and_format() {
        let cmd = parse_args(&[
            "diff".into(),
            "--full".into(),
            "--scope".into(),
            "src/a/**".into(),
            "--scope=src/b/**".into(),
            "--format".into(),
            "dot".into(),
        ])
        .unwrap();
        assert_eq!(
            cmd,
            Cmd::Diff {
                base: "main".to_string(),
                baseline: None,
                show: false,
                full: true,
                scope: vec!["src/a/**".to_string(), "src/b/**".to_string()],
                list: false,
                format: Format::Dot,
            }
        );
    }

    #[test]
    fn parse_unknown_format_errors() {
        assert!(
            parse_args(&["diff".into(), "--format".into(), "bogus".into()]).is_err(),
            "an unknown --format value is a parse error"
        );
        // The unknown value is a usage error, so `run` maps it to exit 2 before touching a repo.
        assert_eq!(
            run(&[
                "diff".to_string(),
                "--format".to_string(),
                "bogus".to_string()
            ]),
            2
        );
    }

    #[test]
    fn scope_narrows_the_rendered_output() {
        let repo = all_views_repo("scope");
        let dir = &repo.path;
        let config = Config::load(&dir.join(".csd.toml")).unwrap();

        // Unscoped: the added module `b` renders in the module view.
        let unscoped = analyze(dir, "main", &config, &RenderOpts::default()).unwrap();
        let unscoped_modules = &unscoped
            .diagrams
            .iter()
            .find(|d| d.view == "modules")
            .unwrap()
            .mermaid;
        assert!(
            unscoped_modules.contains("crate::b"),
            "the added module should render unscoped:\n{unscoped_modules}"
        );

        // Scoped to `src/a/**`: the added module `b` (in src/b.rs) is filtered out.
        let opts = RenderOpts {
            scope: vec!["src/a.rs".to_string()],
            ..RenderOpts::default()
        };
        let scoped = analyze(dir, "main", &config, &opts).unwrap();
        let scoped_modules = &scoped
            .diagrams
            .iter()
            .find(|d| d.view == "modules")
            .unwrap()
            .mermaid;
        assert!(
            !scoped_modules.contains("crate::b"),
            "the out-of-scope module must not render:\n{scoped_modules}"
        );
    }

    /// A repo whose config enables the schema view and denies destructive migrations, with a base
    /// Diesel `schema.rs` whose head drops a column.
    fn schema_repo(tag: &str) -> TmpRepo {
        let repo = TmpRepo::new(tag);
        let dir = &repo.path;
        git_ok(dir, &["-c", "init.defaultBranch=main", "init", "-q"]);
        write(
            dir,
            ".csd.toml",
            "[views]\nenabled = [\"schema\"]\n\n[lint]\ndeny = [\"destructive_migration\"]\n",
        );
        write(dir, "src/lib.rs", "pub fn placeholder() {}\n");
        write(
            dir,
            "src/schema.rs",
            "table! {\n    users (id) {\n        id -> Int4,\n        name -> Varchar,\n    }\n}\n",
        );
        git_ok(dir, &["add", "-A"]);
        git_ok(dir, &["commit", "-q", "-m", "base"]);
        repo
    }

    #[test]
    fn schema_view_renders_and_dropped_column_denies() {
        let repo = schema_repo("schema");
        let dir = &repo.path;
        // Head drops the `name` column: a destructive migration.
        write(
            dir,
            "src/schema.rs",
            "table! {\n    users (id) {\n        id -> Int4,\n    }\n}\n",
        );

        let config = Config::load(&dir.join(".csd.toml")).unwrap();
        let report = analyze(dir, "main", &config, &RenderOpts::default()).unwrap();

        assert_eq!(report.exit_code(), 1, "a dropped column must exit 1");
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.rule == "destructive_migration" && f.message.contains("users.name")),
            "expected a destructive_migration finding, got {:?}",
            report.findings
        );
        let schema = report
            .diagrams
            .iter()
            .find(|d| d.view == "schema")
            .expect("the schema view is enabled");
        assert!(
            schema.mermaid.contains("erDiagram"),
            "an erDiagram is expected:\n{}",
            schema.mermaid
        );
    }

    #[test]
    fn parse_defaults_base_to_main() {
        let cmd = parse_args(&["diff".to_string()]).unwrap();
        assert_eq!(
            cmd,
            Cmd::Diff {
                base: "main".to_string(),
                baseline: None,
                show: false,
                full: false,
                scope: Vec::new(),
                list: false,
                format: Format::Mermaid,
            }
        );
    }

    #[test]
    fn parse_reads_base_both_forms() {
        let split = parse_args(&["diff".into(), "--base".into(), "dev".into()]).unwrap();
        let joined = parse_args(&["diff".into(), "--base=dev".into()]).unwrap();
        let want = Cmd::Diff {
            base: "dev".to_string(),
            baseline: None,
            show: false,
            full: false,
            scope: Vec::new(),
            list: false,
            format: Format::Mermaid,
        };
        assert_eq!(split, want);
        assert_eq!(joined, want);
    }

    #[test]
    fn parse_reads_show_flag() {
        let cmd = parse_args(&["diff".into(), "--show".into()]).unwrap();
        assert_eq!(
            cmd,
            Cmd::Diff {
                base: "main".to_string(),
                baseline: None,
                show: true,
                full: false,
                scope: Vec::new(),
                list: false,
                format: Format::Mermaid,
            }
        );
    }

    #[test]
    fn run_maps_help_and_parse_errors_to_exit_codes() {
        // Help is a clean exit; a missing subcommand or unknown arg is a usage error (2). These
        // paths take no IO, so they pin the exit-code contract without a repo.
        assert_eq!(run(&["--help".to_string()]), 0);
        assert_eq!(run(&[]), 2, "no subcommand is a usage error");
        assert_eq!(
            run(&["bogus".to_string()]),
            2,
            "unknown arg is a usage error"
        );
    }

    #[test]
    fn parse_help_and_errors() {
        assert_eq!(parse_args(&["--help".to_string()]).unwrap(), Cmd::Help);
        assert!(parse_args(&[]).is_err(), "no subcommand is an error");
        assert_eq!(run(&["--version".to_string()]), 0);
        assert_eq!(parse_args(&["-V".to_string()]).unwrap(), Cmd::Version);
        assert!(
            version_string().starts_with("csd "),
            "version line names the binary: {}",
            version_string()
        );
        assert!(
            parse_args(&["bogus".to_string()]).is_err(),
            "unknown arg is an error"
        );
    }

    #[test]
    fn parse_trace_defaults_and_flags() {
        assert_eq!(
            parse_args(&["trace".to_string()]).unwrap(),
            Cmd::Trace {
                base: "main".to_string(),
                cmd: "cargo test".to_string(),
            }
        );
        let cmd = parse_args(&[
            "trace".into(),
            "--base".into(),
            "dev".into(),
            "--cmd".into(),
            "echo hi".into(),
        ])
        .unwrap();
        assert_eq!(
            cmd,
            Cmd::Trace {
                base: "dev".to_string(),
                cmd: "echo hi".to_string(),
            }
        );
    }

    #[test]
    fn extract_only_sequence_mermaid_blocks() {
        let stdout = "\
noise before
```mermaid
sequenceDiagram
    A->>B: hello
```
some prose
```rust
fn not_mermaid() {}
```
```mermaid
flowchart TD
    A --> B
```
trailing noise
";
        let blocks = extract_mermaid_sequences(stdout);
        assert_eq!(
            blocks,
            vec!["sequenceDiagram\n    A->>B: hello".to_string()],
            "only the sequenceDiagram mermaid block is extracted"
        );
    }

    #[test]
    fn trace_delta_added_removed_unchanged() {
        let base = vec!["shared".to_string(), "gone".to_string()];
        let head = vec!["shared".to_string(), "fresh".to_string()];
        let (added, removed) = trace_delta(&base, &head);
        assert_eq!(added, vec!["fresh".to_string()], "head-only is added");
        assert_eq!(removed, vec!["gone".to_string()], "base-only is removed");

        // An identical set has no delta.
        let (a, r) = trace_delta(&base, &base);
        assert!(a.is_empty() && r.is_empty(), "unchanged sets have no delta");
    }

    /// Integration test for the worktree materialization, shell capture, and cleanup guarantee.
    /// The trace command cats a committed file, which differs between base and head, so head gains
    /// one trace and loses another. `sh` and `git` are always available in the sandbox.
    #[test]
    fn observe_traces_diffs_committed_traces_and_cleans_up() {
        let repo = TmpRepo::new("trace");
        let dir = &repo.path;
        git_ok(dir, &["-c", "init.defaultBranch=main", "init", "-q"]);
        write(
            dir,
            "trace.md",
            "```mermaid\nsequenceDiagram\n    A->>B: base\n```\n",
        );
        git_ok(dir, &["add", "-A"]);
        git_ok(dir, &["commit", "-q", "-m", "base"]);
        // Head changes the emitted trace body.
        write(
            dir,
            "trace.md",
            "```mermaid\nsequenceDiagram\n    A->>B: head\n```\n",
        );

        let (base_blocks, head_blocks) = observe_traces(dir, "main", "cat trace.md").unwrap();
        let (added, removed) = trace_delta(&base_blocks, &head_blocks);
        assert_eq!(added, vec!["sequenceDiagram\n    A->>B: head".to_string()]);
        assert_eq!(
            removed,
            vec!["sequenceDiagram\n    A->>B: base".to_string()]
        );

        // No worktree leaked past observe_traces.
        let list = git(dir, &["worktree", "list"]).unwrap();
        assert_eq!(
            list.lines().count(),
            1,
            "only main worktree remains:\n{list}"
        );
    }

    #[test]
    fn parse_reads_list_flag() {
        let cmd = parse_args(&["diff".into(), "--list".into()]).unwrap();
        assert_eq!(
            cmd,
            Cmd::Diff {
                base: "main".to_string(),
                baseline: None,
                show: false,
                full: false,
                scope: Vec::new(),
                list: true,
                format: Format::Mermaid,
            }
        );
    }

    #[test]
    fn parse_doc_defaults_and_out() {
        assert_eq!(
            parse_args(&["doc".to_string()]).unwrap(),
            Cmd::Doc { out: None }
        );
        let split = parse_args(&["doc".into(), "-o".into(), "out.md".into()]).unwrap();
        let long = parse_args(&["doc".into(), "--out".into(), "out.md".into()]).unwrap();
        let joined = parse_args(&["doc".into(), "--out=out.md".into()]).unwrap();
        let want = Cmd::Doc {
            out: Some("out.md".to_string()),
        };
        assert_eq!(split, want);
        assert_eq!(long, want);
        assert_eq!(joined, want);
    }

    #[test]
    fn change_list_reports_added_removed_and_member_delta() {
        let repo = TmpRepo::new("list");
        let dir = &repo.path;
        git_ok(dir, &["-c", "init.defaultBranch=main", "init", "-q"]);
        write(dir, ".csd.toml", "[views]\nenabled = [\"types\"]\n");
        write(
            dir,
            "src/lib.rs",
            "pub struct Order { pub id: u64 }\npub struct Gone { pub x: u64 }\n",
        );
        git_ok(dir, &["add", "-A"]);
        git_ok(dir, &["commit", "-q", "-m", "base"]);
        // Head: add a field to Order, remove the Gone struct, add a free fn.
        write(
            dir,
            "src/lib.rs",
            "pub struct Order { pub id: u64, pub total: u64 }\npub fn added_fn() {}\n",
        );

        let config = Config::load(&dir.join(".csd.toml")).unwrap();
        let report = analyze(dir, "main", &config, &RenderOpts::default()).unwrap();
        let lines = change_list(&report.changes);

        assert!(
            lines.contains(&"+ fn crate::added_fn".to_string()),
            "an added fn line is expected:\n{lines:#?}"
        );
        assert!(
            lines.contains(&"- struct crate::Gone".to_string()),
            "a removed struct line is expected:\n{lines:#?}"
        );
        assert!(
            lines.contains(&"~ struct crate::Order (+total)".to_string()),
            "a modified struct with member detail is expected:\n{lines:#?}"
        );

        // Deterministic: a second analyze produces the same list.
        let again = analyze(dir, "main", &config, &RenderOpts::default()).unwrap();
        assert_eq!(lines, change_list(&again.changes), "the list is stable");
    }

    #[test]
    fn doc_snapshot_has_title_module_index_and_mermaid_per_view() {
        let repo = all_views_repo("doc");
        let dir = &repo.path;
        let config = Config::load(&dir.join(".csd.toml")).unwrap();
        let head = extract_head(dir).unwrap();
        let doc = doc_markdown(&head, &config);

        assert!(
            doc.starts_with("# "),
            "the report opens with an H1 title:\n{doc}"
        );
        assert!(
            doc.contains("## Module index"),
            "a module index section is expected:\n{doc}"
        );
        assert!(
            doc.contains("- `crate::a`"),
            "the module index lists a module id:\n{doc}"
        );
        // One ```mermaid block per enabled view (six here).
        let fences = doc.matches("```mermaid").count();
        assert_eq!(fences, 6, "one mermaid block per enabled view:\n{doc}");

        // Writing to -o creates the file with the report contents.
        let out = dir.join("report.md");
        emit_doc(&doc, Some(&out.to_string_lossy())).unwrap();
        let written = std::fs::read_to_string(&out).unwrap();
        assert_eq!(written, doc, "the -o file holds the whole report");
    }

    fn cl_node(id: &str, kind: NodeKind) -> csd_ir::Node {
        csd_ir::Node {
            id: StableId::new(id),
            kind,
            span: csd_ir::SourceSpan {
                file: "src/lib.rs".into(),
                start: 0,
                end: 1,
            },
            attrs: std::collections::BTreeMap::new(),
            fingerprint: None,
        }
    }

    #[test]
    fn changelog_groups_by_module_and_titles_sections() {
        let changes = vec![
            Change::Added(cl_node("crate::billing::Invoice", NodeKind::Struct)),
            Change::Removed(cl_node("crate::billing::legacy_charge", NodeKind::Fn)),
            Change::Added(cl_node("crate::payments::Card", NodeKind::Struct)),
            Change::EdgeAdded(csd_ir::Edge {
                from: StableId::new("crate::billing"),
                to: StableId::new("crate::payments"),
                kind: EdgeKind::Uses,
                span: cl_node("x", NodeKind::Module).span,
                ordinal: None,
            }),
        ];
        let md = changelog_markdown("main", &changes);

        assert!(md.starts_with("# Structure changelog vs main\n"));
        // Modules are grouped and alphabetised by the BTreeMap.
        let billing = md.find("## crate::billing").expect("billing section");
        let payments = md.find("## crate::payments").expect("payments section");
        let deps = md.find("## Dependencies").expect("dependencies section");
        assert!(billing < payments, "modules sort alphabetically");
        assert!(payments < deps, "dependencies come last");
        // Short names, not full paths, under the owning module.
        assert!(md.contains("### Added\n- struct `Invoice`"));
        assert!(md.contains("### Removed\n- fn `legacy_charge`"));
        assert!(md.contains("- + `crate::billing` -> `crate::payments` (uses)"));
    }

    #[test]
    fn changelog_tags_a_modified_module_on_its_heading_not_as_a_bullet() {
        let changes = vec![
            Change::Modified {
                before: cl_node("crate::billing", NodeKind::Module),
                after: cl_node("crate::billing", NodeKind::Module),
            },
            Change::Added(cl_node("crate::billing::Invoice", NodeKind::Struct)),
        ];
        let md = changelog_markdown("main", &changes);

        // The module is tagged on its own heading, once.
        assert!(
            md.contains("## crate::billing (modified)"),
            "module change should tag the heading:\n{md}"
        );
        // It must NOT also appear as a "Modified module" bullet under the crate root.
        assert!(
            !md.contains("module `billing`"),
            "a changed module must not double-list as a bullet:\n{md}"
        );
        // No empty crate-root section is emitted.
        assert!(
            !md.contains("## (crate root)"),
            "no empty parent section:\n{md}"
        );
        assert!(md.contains("### Added\n- struct `Invoice`"));
    }

    #[test]
    fn init_generates_a_valid_config_with_detected_globs() {
        let mut graph = Graph::new();
        let mut m = cl_node("crate::thing", NodeKind::Module);
        m.span.file = "crates/foo/src/lib.rs".into();
        graph.nodes = vec![m];

        let toml = init_config(&graph);
        // The detected crate glob is present (commented, for the user to assign a layer).
        assert!(
            toml.contains("crates/foo/**"),
            "generated config should carry the detected glob:\n{toml}"
        );
        // The generated config parses as valid, so `csd init` never emits a broken file.
        csd_config::Config::parse(&toml).expect("generated config must be valid");
    }

    #[test]
    fn files_index_groups_definitions_by_file_with_member_types() {
        let mut graph = Graph::new();
        let mut order = cl_node("crate::billing::Order", NodeKind::Struct);
        order.span.file = "src/billing.rs".into();
        order.fingerprint = Some(csd_ir::Fingerprint {
            member_types: [("id", "u64"), ("total", "Money")]
                .iter()
                .map(|(m, t)| (m.to_string(), t.to_string()))
                .collect(),
            ..Default::default()
        });
        let mut charge = cl_node("crate::billing::charge", NodeKind::Fn);
        charge.span.file = "src/billing.rs".into();
        charge
            .attrs
            .insert("sig".into(), "(amount: Money) -> bool".into());
        graph.nodes = vec![order, charge];
        graph.normalize();

        let md = files_markdown(&graph);
        assert!(md.contains("## `src/billing.rs`"));
        assert!(
            md.contains("struct `Order` { id: u64, total: Money }"),
            "type line should carry member types:\n{md}"
        );
        assert!(
            md.contains("### Functions\n- `charge(amount: Money) -> bool`"),
            "function line should carry the captured signature:\n{md}"
        );
    }

    #[test]
    fn changelog_reports_nothing_on_an_empty_delta() {
        let md = changelog_markdown("dev", &[]);
        assert_eq!(
            md,
            "# Structure changelog vs dev\n\nNo structural changes.\n"
        );
    }
}
