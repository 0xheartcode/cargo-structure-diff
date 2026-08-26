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

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{bail, Context, Result};
use csd_config::Config;
use csd_diff::{diff, DiffOptions, FileRename};
use csd_ir::{Change, EdgeKind, Graph, StableId};
use csd_lint::{has_denials, lint, Finding, Severity};
use csd_render::{render_call_view, render_module_view, render_state_view, render_type_view};

/// Default base ref when `--base` is omitted.
const DEFAULT_BASE: &str = "main";

/// Default trace command when `--cmd` is omitted.
const DEFAULT_TRACE_CMD: &str = "cargo test";

/// Usage text for `-h`/`--help` and parse errors.
const HELP: &str = "\
cargo-structure-diff: structural diff and architectural lint gate

USAGE:
    csd diff [--base <ref>] [--show]
    csd trace [--base <ref>] [--cmd <shell command>]
    cargo structure-diff diff [--base <ref>] [--show]

OPTIONS:
    --base <ref>    Base git ref to diff against (default: main)
    --show          diff: print every rendered view even when nothing denies
    --cmd <cmd>     trace: shell command that emits Mermaid (default: cargo test)
    -h, --help      Print this help

trace mode is opt-in and observational: it runs <cmd> in the base and head
worktrees, extracts the emitted `sequenceDiagram` Mermaid blocks, and diffs the
observed traces as a set. It exits 0 (2 only if the command fails to run).
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
        /// Print every rendered view even when nothing denies (for interactive use).
        show: bool,
    },
    /// Run a trace command in base and head, then diff the observed Mermaid traces.
    Trace {
        /// The git ref to treat as the base side.
        base: String,
        /// The shell command that emits Mermaid on stdout.
        cmd: String,
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
    let mut cmd = DEFAULT_TRACE_CMD.to_string();
    let mut show = false;
    let mut sub: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "-h" | "--help" => return Ok(Cmd::Help),
            "--show" => show = true,
            "diff" | "trace" if sub.is_none() => sub = Some(arg),
            "--base" => {
                i += 1;
                let value = args.get(i).context("--base requires a value")?;
                base = value.clone();
            }
            _ if arg.starts_with("--base=") => {
                base = arg["--base=".len()..].to_string();
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
        Some(_) => Ok(Cmd::Diff { base, show }),
        None => bail!("expected the `diff` or `trace` subcommand; try --help"),
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
        Cmd::Diff { base, show } => match run_diff(&base, show) {
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
    }
}

/// Resolve the repo root, load config, run the pipeline, and print the report.
fn run_diff(base: &str, show: bool) -> Result<i32> {
    let repo = repo_root()?;
    let config = load_config(&repo)?;
    let report = analyze(&repo, base, &config)?;
    report.print(base, show);
    Ok(report.exit_code())
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
pub fn analyze(repo: &Path, base: &str, config: &Config) -> Result<Report> {
    let base_graph = base_graph(repo, base)?;
    let head_graph =
        csd_extract_rs::extract(repo).context("failed to extract head working tree")?;
    let file_renames = file_renames(repo, base)?;
    let changes = diff(
        &base_graph,
        &head_graph,
        DiffOptions {
            rename_threshold: Some(0.7),
            file_renames,
        },
    );
    let findings = lint(&head_graph, &changes, config);
    let diagrams = render_views(&head_graph, &changes, config);
    Ok(Report {
        changes,
        findings,
        diagrams,
    })
}

/// Render one diagram per enabled view. Defaults to the module view when `views.enabled` is empty;
/// unknown view names are ignored. The state lints run via [`lint`] independent of this.
fn render_views(head: &Graph, changes: &[Change], config: &Config) -> Vec<ViewDiagram> {
    let enabled: Vec<String> = if config.views.enabled.is_empty() {
        vec!["modules".to_string()]
    } else {
        config.views.enabled.clone()
    };
    let mut diagrams = Vec::new();
    for view in enabled {
        let mermaid = match view.as_str() {
            "modules" => render_module_view(head, changes),
            "states" => render_state_view(head, changes),
            "types" => render_type_view(head, changes),
            "calls" => render_call_view(head, changes, &call_entry(head, config)),
            _ => continue,
        };
        diagrams.push(ViewDiagram { view, mermaid });
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
    let result =
        csd_extract_rs::extract(&worktree).with_context(|| format!("failed to extract {base:?}"));
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
        let report = analyze(dir, "main", &config).unwrap();

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
        let report = analyze(dir, "main", &config).unwrap();

        assert_eq!(report.exit_code(), 0, "no denial must exit 0");
        assert!(
            !report.denied(),
            "no denied findings expected, got {:?}",
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
        let report = analyze(dir, "main", &config).unwrap();

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
        let report = analyze(dir, "main", &config).unwrap();

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
        let report = analyze(dir, "main", &config).unwrap();

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

    #[test]
    fn parse_defaults_base_to_main() {
        let cmd = parse_args(&["diff".to_string()]).unwrap();
        assert_eq!(
            cmd,
            Cmd::Diff {
                base: "main".to_string(),
                show: false,
            }
        );
    }

    #[test]
    fn parse_reads_base_both_forms() {
        let split = parse_args(&["diff".into(), "--base".into(), "dev".into()]).unwrap();
        let joined = parse_args(&["diff".into(), "--base=dev".into()]).unwrap();
        let want = Cmd::Diff {
            base: "dev".to_string(),
            show: false,
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
                show: true,
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
}
