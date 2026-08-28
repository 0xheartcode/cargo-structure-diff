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

/// Default trace command when `--cmd` is omitted.
const DEFAULT_TRACE_CMD: &str = "cargo test";

/// Usage text for `-h`/`--help` and parse errors.
const HELP: &str = "\
cargo-structure-diff: structural diff and architectural lint gate

USAGE:
    csd diff [--base <ref>|--baseline <file>] [--show] [--full] [--list] [--scope <glob>]... [--format <fmt>]
    csd snapshot [-o <file>|--out <file>]
    csd changelog [--base <ref>]
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
    --cmd <cmd>        trace: shell command that emits Mermaid (default: cargo test)
    -o, --out <f>      doc/snapshot: write output to <file> instead of stdout
    -h, --help         Print this help

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
";

/// A parsed invocation.
#[derive(Debug, PartialEq, Eq)]
pub enum Cmd {
    /// Print usage and exit zero.
    Help,
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
            "--show" => show = true,
            "--full" => full = true,
            "--list" => list = true,
            "diff" | "trace" | "doc" | "changelog" | "snapshot" if sub.is_none() => sub = Some(arg),
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
        Some("changelog") => Ok(Cmd::Changelog { base }),
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
    let mut edges: Vec<String> = Vec::new();

    for change in changes {
        match change {
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
        let _ = writeln!(out, "## {module}");
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
    fn changelog_reports_nothing_on_an_empty_delta() {
        let md = changelog_markdown("dev", &[]);
        assert_eq!(
            md,
            "# Structure changelog vs dev\n\nNo structural changes.\n"
        );
    }
}
