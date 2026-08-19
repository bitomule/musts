//! Static authoring checks for `MUSTS.yml`.
//!
//! `validate` answers "is the work done". `lint` answers "is this
//! manifest going to cost more than it is worth" — a question that only
//! shows up months later as an agent bill.
//!
//! Every rule here comes from a manifest that was really written, and
//! each one describes the same failure: work that a deterministic runner
//! or a `paths:` filter would have done for free is instead handed to an
//! agent to reason about and write prose about, on every change, forever.
//!
//! Where a rule can be checked against the real tree it is — the glob
//! rules report the files that actually match today rather than guessing
//! from the pattern's shape.

use std::collections::BTreeMap;
use std::path::Path;

use globset::GlobBuilder;
use serde::Serialize;

use crate::error::Result;
use crate::manifest::{check_id, scope_path_for, Check};

/// How bad a finding is. `lint` exits non-zero when any `Error` is
/// present, so a repo can gate CI on it without also gating on advice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Advice: the manifest works, but costs more than it needs to.
    Warning,
    /// The manifest does not do what it says.
    Error,
}

/// One authoring problem.
#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub severity: Severity,
    /// Workspace-relative manifest path.
    pub manifest: String,
    /// Fully-qualified check id, or `None` for a file-level finding.
    pub check: Option<String>,
    /// Stable kebab-case rule name, so a repo can grep for one.
    pub rule: &'static str,
    pub message: String,
}

/// Everything `musts lint` found.
#[derive(Debug, Clone, Serialize)]
pub struct LintReport {
    pub findings: Vec<Finding>,
}

impl LintReport {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    pub fn has_errors(&self) -> bool {
        self.findings.iter().any(|f| f.severity == Severity::Error)
    }
}

/// Lint every manifest in the workspace.
pub fn run(workspace_root: &Path) -> Result<LintReport> {
    let files = workspace_files(workspace_root);
    let ignored_files = workspace_files_including_ignored(workspace_root);
    let mut findings = Vec::new();

    for entry in crate::manifest::discover(workspace_root)? {
        let bytes = match std::fs::read(&entry.abs_path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let manifest = match crate::manifest::parse(&entry.rel_path, &bytes) {
            Ok(m) => m,
            // A manifest that does not parse is `validate`'s problem, not
            // lint's — it already fails loudly there with a better message.
            Err(_) => continue,
        };
        let manifest_rel = entry.rel_path.display().to_string();
        let scope = scope_path_for(&entry.rel_path);
        let allowed = suppressed_rules(&bytes);

        let mut for_this_manifest = Vec::new();
        for w in &manifest.warnings {
            for_this_manifest.push(Finding {
                severity: Severity::Error,
                manifest: manifest_rel.clone(),
                check: w.check_local_id.as_ref().map(|id| check_id(&scope, id)),
                rule: "unknown-key",
                message: w.to_string(),
            });
        }

        for (local_id, check) in &manifest.checks {
            let cid = check_id(&scope, local_id);
            lint_check(
                &manifest_rel,
                &cid,
                check,
                &entry.rel_path,
                &files,
                &ignored_files,
                &mut for_this_manifest,
            );
        }

        for_this_manifest.retain(|f| !allowed.contains(f.rule));
        findings.extend(for_this_manifest);
    }

    findings.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.manifest.cmp(&b.manifest))
            .then_with(|| a.check.cmp(&b.check))
    });
    Ok(LintReport { findings })
}

#[allow(clippy::too_many_arguments)]
fn lint_check(
    manifest_rel: &str,
    cid: &str,
    check: &Check,
    manifest_path: &Path,
    files: &[String],
    ignored_files: &[String],
    out: &mut Vec<Finding>,
) {
    let scope_dir = manifest_path
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let push = |out: &mut Vec<Finding>, severity, rule, message: String| {
        out.push(Finding {
            severity,
            manifest: manifest_rel.to_string(),
            check: Some(cid.to_string()),
            rule,
            message,
        })
    };

    if check.uses == "agent" {
        for fact in agent_facts(check) {
            if let Some(command) = deterministic_command(&fact) {
                push(
                    out,
                    Severity::Warning,
                    "deterministic-fact-under-agent",
                    format!(
                        "fact asserts a command's exit status ({command}), which is not judgment. \
                         An agent has to run it, read it, and write prose about it on every \
                         change. Use a runnable capability (`bazel/test`, `cargo/*`) — `musts \
                         run` then executes it and records the real result for free."
                    ),
                );
            }
            if let Some(quoted) = prose_path_condition(&fact) {
                push(
                    out,
                    Severity::Warning,
                    "paths-written-as-prose",
                    format!(
                        "fact encodes a path condition in prose ({quoted}). Every unrelated \
                         change pays an agent to read it, decide it does not apply, and submit \
                         evidence saying so. Move the condition into `paths:` and the check will \
                         not fire at all."
                    ),
                );
            }
        }
    }

    // Only for judgment capabilities. A root `cargo/test` with no
    // `paths:` is correct *and* cheap — `musts run` executes it and the
    // agent never reads the output. The same check under `uses: agent`
    // bills a round of reasoning and prose for every file in the folder,
    // which is the thing worth seeing.
    if check.paths.is_empty() && is_judgment(&check.uses) {
        let where_ = if scope_dir.is_empty() {
            "the workspace root".to_string()
        } else {
            format!("`{scope_dir}/`")
        };
        let in_scope = files.iter().filter(|f| under(f, &scope_dir)).count();
        push(
            out,
            Severity::Warning,
            "no-paths-filter",
            format!(
                "no `paths:`, so this fires on every change under {where_} — {in_scope} file(s) \
                 today. If it only cares about some of them, say so in `paths:`."
            ),
        );
    }

    for pat in check.paths.iter().chain(check.exclude_paths.iter()) {
        let Ok(matcher) = GlobBuilder::new(pat)
            .case_insensitive(true)
            .build()
            .map(|g| g.compile_matcher())
        else {
            // An invalid glob is a hard manifest error in `validate`, with
            // a better message than lint could give.
            continue;
        };
        let ctx = GlobContext {
            files,
            ignored_files,
            scope_dir: &scope_dir,
            matcher,
        };
        for finding in glob_surprises(pat, &ctx) {
            push(out, Severity::Warning, finding.0, finding.1);
        }
    }
}

/// Rules the manifest opts out of, via `# musts-lint: allow <rule>`.
///
/// A correct finding can still be noise. One repo's manifest opens with a
/// header comment deliberately reasoning about `*` crossing `/` and
/// constructing two globs to be disjoint *because* of it — the
/// `glob-crosses-directories` warning there is accurate and unwanted, and
/// a lint nobody can silence is a lint everybody ignores.
///
/// Scope is the whole file, on purpose. Per-check suppression means
/// binding a comment to a YAML node, and `serde_yaml` discards comments —
/// reconstructing that from raw text is guesswork that would break on
/// reformatting. File scope is honest about what it can promise.
///
/// Read from the raw bytes for the same reason.
fn suppressed_rules(manifest_bytes: &[u8]) -> std::collections::BTreeSet<String> {
    const MARKER: &str = "musts-lint:";
    let mut out = std::collections::BTreeSet::new();
    let Ok(text) = std::str::from_utf8(manifest_bytes) else {
        return out;
    };
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('#') {
            continue;
        }
        let Some(rest) = trimmed.trim_start_matches('#').trim().strip_prefix(MARKER) else {
            continue;
        };
        let Some(rules) = rest.trim().strip_prefix("allow") else {
            continue;
        };
        for rule in rules.split([',', ' ']) {
            let rule = rule.trim();
            if !rule.is_empty() {
                out.insert(rule.to_string());
            }
        }
    }
    out
}

/// Does satisfying this capability cost an agent's reasoning, rather
/// than a command musts can run itself?
///
/// Third-party capabilities are treated as runnable: musts cannot know
/// what they cost, and guessing wrong here means noise on every run of
/// every repo that ships an extension.
fn is_judgment(uses: &str) -> bool {
    uses == "agent" || uses.starts_with("mav/")
}

/// Facts declared by an `agent` check's `with` payload.
fn agent_facts(check: &Check) -> Vec<String> {
    check
        .with_payload
        .get("facts")
        .and_then(|f| f.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Does this fact assert a command's exit status rather than a judgment?
/// Returns a short quote for the message.
fn deterministic_command(fact: &str) -> Option<String> {
    let lower = fact.to_lowercase();
    for marker in ["exited 0", "exit 0", "exits 0", "exit code 0", "returns 0"] {
        if lower.contains(marker) {
            return Some(format!("\"{marker}\""));
        }
    }
    // `Run `cmd` and confirm …` — the shape HiddenFace uses for both of
    // its checks.
    if lower.contains("run `") && (lower.contains("and confirm") || lower.contains("and verify")) {
        return Some("\"Run `…` and confirm …\"".to_string());
    }
    None
}

/// Does this fact encode a `paths:` condition in prose?
fn prose_path_condition(fact: &str) -> Option<String> {
    let lower = fact.to_lowercase();
    for marker in [
        "if changes touch",
        "if the changes touch",
        "if changes are unrelated",
        "trivially satisfied",
        "does not apply",
    ] {
        if lower.contains(marker) {
            return Some(format!("\"{marker}…\""));
        }
    }
    None
}

/// Ways a pattern matches more than it looks like it does, reported
/// against the files that are really there rather than from the
/// pattern's shape alone.
/// Everything the glob rules need beyond the pattern itself.
struct GlobContext<'a> {
    /// Files musts can see (ignore rules applied) — the same set the
    /// validator scopes over.
    files: &'a [String],
    /// Files on disk that ignore rules hide. Only ever used to explain an
    /// empty match.
    ignored_files: &'a [String],
    /// Workspace-relative folder of the declaring manifest, `""` at root.
    scope_dir: &'a str,
    /// The pattern compiled case-insensitively, shared by the rules.
    matcher: globset::GlobMatcher,
}

fn glob_surprises(pattern: &str, ctx: &GlobContext<'_>) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let files = ctx.files;
    let insensitive = &ctx.matcher;
    let matched: Vec<&String> = files
        .iter()
        .filter(|f| insensitive.is_match(f.as_str()))
        .collect();

    // Files that match only because matching is case-insensitive. This is
    // the `*View.swift` / `RequestReview.swift` trap, which one repo
    // documented in a 15-line header comment rather than being told.
    if let Ok(sensitive) = GlobBuilder::new(pattern)
        .case_insensitive(false)
        .build()
        .map(|g| g.compile_matcher())
    {
        let case_only: Vec<&&String> = matched
            .iter()
            .filter(|f| !sensitive.is_match(f.as_str()))
            .collect();
        if let Some(example) = case_only.first() {
            out.push((
                "glob-case-insensitive",
                format!(
                    "`{pattern}` matches {} file(s) only because musts globs are \
                     case-insensitive, e.g. `{example}`. Narrow the pattern if that is not \
                     intended.",
                    case_only.len()
                ),
            ));
        }
    }

    // Files that match only because `*` crosses `/`. Authors read `*` as
    // "within one directory" — it is not.
    if pattern.contains('*') {
        if let Ok(literal) = GlobBuilder::new(pattern)
            .case_insensitive(true)
            .literal_separator(true)
            .build()
            .map(|g| g.compile_matcher())
        {
            let crossing: Vec<&&String> = matched
                .iter()
                .filter(|f| !literal.is_match(f.as_str()))
                .collect();
            if let Some(example) = crossing.first() {
                out.push((
                    "glob-crosses-directories",
                    format!(
                        "`{pattern}` matches {} file(s) only because `*` and `**` both cross \
                         `/` in musts globs, e.g. `{example}`. If you meant one directory \
                         level, list the paths explicitly.",
                        crossing.len()
                    ),
                ));
            }
        }
    }

    if matched.is_empty() {
        out.push(("glob-matches-nothing", explain_empty_match(pattern, ctx)));
    }

    out
}

/// Say *why* a glob matches nothing. "Check for a typo" is the least
/// likely cause and the least useful steer.
///
/// The case this exists for: a repo's `.mustsignore` said `*.png`,
/// intending root-level screenshots. Unanchored, it also hid 122 committed
/// snapshot baselines, so the snapshot check could not see the files it
/// existed to protect — re-recording a baseline did not reopen it. The
/// old message pointed at a typo, and the glob was fine.
fn explain_empty_match(pattern: &str, ctx: &GlobContext<'_>) -> String {
    let hidden: Vec<&String> = ctx
        .ignored_files
        .iter()
        .filter(|f| ctx.matcher.is_match(f.as_str()))
        .collect();
    if let Some(example) = hidden.first() {
        return format!(
            "`{pattern}` matches no file musts can see, so the check never fires — but {} file(s) \
             at that path exist and are excluded by `.gitignore` or `.mustsignore`, e.g. \
             `{example}`. The check is blind to them: editing one will not re-open it. Narrow the \
             ignore rule (an unanchored `*.ext` matches at every depth; `/*.ext` is root-only).",
            hidden.len()
        );
    }

    // `paths:` globs are workspace-relative even inside a nested manifest,
    // which reads as a surprise often enough to be worth naming.
    if !ctx.scope_dir.is_empty() {
        let prefixed = format!("{}/{}", ctx.scope_dir, pattern);
        if let Ok(m) = GlobBuilder::new(&prefixed)
            .case_insensitive(true)
            .build()
            .map(|g| g.compile_matcher())
        {
            if ctx.files.iter().any(|f| m.is_match(f.as_str())) {
                return format!(
                    "`{pattern}` matches no file, but `{prefixed}` would. `paths:` globs are \
                     relative to the **workspace root**, not to this manifest's folder."
                );
            }
        }
    }

    format!(
        "`{pattern}` matches no file in the workspace today, so the check never fires. Check for \
         a typo."
    )
}

fn under(file: &str, dir: &str) -> bool {
    dir.is_empty() || file.starts_with(&format!("{dir}/"))
}

/// Workspace-relative paths of every file `validate` would consider,
/// honouring `.gitignore` and `.mustsignore` the same way.
fn workspace_files(workspace_root: &Path) -> Vec<String> {
    walk(workspace_root, true)
}

/// Every file on disk, ignore rules **not** applied. Used only to explain
/// a glob that matches nothing: "no such file" and "the file is there but
/// `.mustsignore` hides it" are different problems with different fixes,
/// and the second is the one that silently under-scopes a check.
fn workspace_files_including_ignored(workspace_root: &Path) -> Vec<String> {
    walk(workspace_root, false)
}

fn walk(workspace_root: &Path, honour_ignores: bool) -> Vec<String> {
    let mut builder = ignore::WalkBuilder::new(workspace_root);
    builder
        .standard_filters(honour_ignores)
        .git_ignore(honour_ignores)
        .git_exclude(honour_ignores)
        .git_global(honour_ignores)
        .ignore(honour_ignores)
        .parents(honour_ignores)
        .require_git(false)
        .hidden(false);
    // `add_custom_ignore_filename` has no on/off switch — a registered
    // custom ignore file is always applied — so the un-ignored walk must
    // simply never register it.
    if honour_ignores {
        builder.add_custom_ignore_filename(".mustsignore");
    }
    let mut out = Vec::new();
    for entry in builder.build().flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path: &Path = entry.path();
        if path
            .components()
            .any(|c| c.as_os_str() == ".git" || c.as_os_str() == ".musts")
        {
            continue;
        }
        if let Ok(rel) = path.strip_prefix(workspace_root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
    out.sort();
    out
}

/// Render for humans.
pub fn render_text(report: &LintReport) -> String {
    if report.is_clean() {
        return "No authoring problems found.\n".to_string();
    }
    let mut out = String::new();
    let mut by_manifest: BTreeMap<&str, Vec<&Finding>> = BTreeMap::new();
    for f in &report.findings {
        by_manifest.entry(&f.manifest).or_default().push(f);
    }
    for (manifest, findings) in &by_manifest {
        out.push_str(&format!("{manifest}\n"));
        for f in findings {
            let label = match f.severity {
                Severity::Error => "error",
                Severity::Warning => "warn ",
            };
            match &f.check {
                Some(c) => out.push_str(&format!("  {label}  {c}  [{}]\n", f.rule)),
                None => out.push_str(&format!("  {label}  [{}]\n", f.rule)),
            }
            out.push_str(&format!("         {}\n", f.message));
        }
        out.push('\n');
    }
    let errors = report
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .count();
    out.push_str(&format!(
        "{} finding(s), {} error(s).\n",
        report.findings.len(),
        errors
    ));
    out
}

pub fn render_json(report: &LintReport) -> String {
    serde_json::to_string_pretty(report).unwrap_or_else(|_| "{}".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ws(manifest: &str, files: &[&str]) -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("MUSTS.yml"), manifest).unwrap();
        for f in files {
            let p = dir.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, "x\n").unwrap();
        }
        dir
    }

    fn rules(report: &LintReport) -> Vec<&str> {
        report.findings.iter().map(|f| f.rule).collect()
    }

    /// The exact fact HiddenFace writes, twice.
    #[test]
    fn flags_a_fact_that_only_asserts_an_exit_code() {
        let dir = ws(
            "version: 1\nchecks:\n  unit:\n    uses: agent\n    paths: [\"src/**\"]\n    with:\n      facts:\n        - \"Run `bazelisk test //T:T --test_output=errors` and confirm all unit tests pass (exit 0).\"\n",
            &["src/a.swift"],
        );
        let r = run(dir.path()).unwrap();
        assert!(
            rules(&r).contains(&"deterministic-fact-under-agent"),
            "{:?}",
            r.findings
        );
        let f = r
            .findings
            .iter()
            .find(|f| f.rule == "deterministic-fact-under-agent")
            .unwrap();
        assert!(f.message.contains("musts run"), "{}", f.message);
    }

    /// The exact shape Nokoru's `live-diarization-matrix` and
    /// `device-speech-harness` use.
    #[test]
    fn flags_a_path_condition_written_as_prose() {
        let dir = ws(
            "version: 1\nchecks:\n  matrix:\n    uses: agent\n    paths: [\"src/**\"]\n    with:\n      facts:\n        - \"If changes touch src/Transcription/**, the live matrix was re-run.\"\n        - \"If changes are unrelated to those paths, this check is trivially satisfied.\"\n",
            &["src/a.swift"],
        );
        let r = run(dir.path()).unwrap();
        let hits = r
            .findings
            .iter()
            .filter(|f| f.rule == "paths-written-as-prose")
            .count();
        assert_eq!(
            hits, 2,
            "both facts encode a path condition: {:?}",
            r.findings
        );
    }

    #[test]
    fn a_genuine_judgment_fact_is_not_flagged() {
        let dir = ws(
            "version: 1\nchecks:\n  review:\n    uses: agent\n    paths: [\"src/**\"]\n    with:\n      facts:\n        - \"Every new public type has a doc comment explaining why it exists.\"\n",
            &["src/a.swift"],
        );
        let r = run(dir.path()).unwrap();
        assert!(
            !rules(&r).contains(&"deterministic-fact-under-agent"),
            "{:?}",
            r.findings
        );
        assert!(
            !rules(&r).contains(&"paths-written-as-prose"),
            "{:?}",
            r.findings
        );
    }

    #[test]
    fn reports_effective_scope_for_a_check_with_no_paths() {
        let dir = ws(
            "version: 1\nchecks:\n  everything:\n    uses: agent\n    with:\n      facts: [\"f\"]\n",
            &["src/a.swift", "src/b.swift", "docs/c.md"],
        );
        let r = run(dir.path()).unwrap();
        let f = r
            .findings
            .iter()
            .find(|f| f.rule == "no-paths-filter")
            .unwrap();
        assert!(f.message.contains("every change"), "{}", f.message);
        // Names the real count so the author sees the blast radius.
        assert!(f.message.contains("file(s) today"), "{}", f.message);
    }

    /// The `*View.swift` / `RequestReview.swift` collision that one repo
    /// documented in a 15-line header comment instead of being told.
    #[test]
    fn flags_a_case_insensitive_glob_collision_with_the_real_file() {
        let dir = ws(
            "version: 1\nchecks:\n  snapshot:\n    uses: agent\n    paths: [\"src/*View.swift\"]\n    with:\n      facts: [\"f\"]\n",
            &["src/HomeView.swift", "src/RequestReview.swift"],
        );
        let r = run(dir.path()).unwrap();
        let f = r
            .findings
            .iter()
            .find(|f| f.rule == "glob-case-insensitive")
            .unwrap_or_else(|| panic!("{:?}", r.findings));
        assert!(
            f.message.contains("RequestReview.swift"),
            "must name the real file: {}",
            f.message
        );
    }

    #[test]
    fn flags_a_star_that_crosses_directories() {
        let dir = ws(
            "version: 1\nchecks:\n  ui:\n    uses: agent\n    paths: [\"src/*.swift\"]\n    with:\n      facts: [\"f\"]\n",
            &["src/a.swift", "src/deep/nested/b.swift"],
        );
        let r = run(dir.path()).unwrap();
        let f = r
            .findings
            .iter()
            .find(|f| f.rule == "glob-crosses-directories")
            .unwrap_or_else(|| panic!("{:?}", r.findings));
        assert!(f.message.contains("deep/nested/b.swift"), "{}", f.message);
    }

    #[test]
    fn flags_a_glob_that_matches_nothing() {
        let dir = ws(
            "version: 1\nchecks:\n  typo:\n    uses: agent\n    paths: [\"srcc/**\"]\n    with:\n      facts: [\"f\"]\n",
            &["src/a.swift"],
        );
        let r = run(dir.path()).unwrap();
        assert!(
            rules(&r).contains(&"glob-matches-nothing"),
            "{:?}",
            r.findings
        );
    }

    #[test]
    fn an_unknown_key_is_an_error_not_a_warning() {
        let dir = ws(
            "version: 1\nchecks:\n  c:\n    uses: agent\n    paths: [\"src/**\"]\n    excludes: [\"src/gen/**\"]\n    with:\n      facts: [\"f\"]\n",
            &["src/a.swift"],
        );
        let r = run(dir.path()).unwrap();
        let f = r.findings.iter().find(|f| f.rule == "unknown-key").unwrap();
        assert_eq!(f.severity, Severity::Error);
        assert!(r.has_errors());
        assert!(f.message.contains("exclude_paths"), "{}", f.message);
    }

    #[test]
    fn a_well_written_manifest_is_clean() {
        let dir = ws(
            "version: 1\nchecks:\n  unit:\n    uses: cargo/test\n    paths: [\"src/**/*.rs\"]\n",
            &["src/a.rs"],
        );
        let r = run(dir.path()).unwrap();
        assert!(r.is_clean(), "{:?}", r.findings);
        assert_eq!(render_text(&r), "No authoring problems found.\n");
    }

    #[test]
    fn errors_sort_above_warnings() {
        let dir = ws(
            "version: 1\nchecks:\n  c:\n    uses: agent\n    excludes: [\"x\"]\n    with:\n      facts: [\"f\"]\n",
            &["src/a.swift"],
        );
        let r = run(dir.path()).unwrap();
        assert_eq!(r.findings[0].severity, Severity::Error);
    }

    /// A runnable check with no `paths:` is correct and cheap: `musts
    /// run` executes it and no agent reads the output. Warning there is
    /// noise on every root manifest in every repo.
    #[test]
    fn a_runnable_check_with_no_paths_is_not_flagged() {
        let dir = ws(
            "version: 1\nchecks:\n  test:\n    uses: cargo/test\n",
            &["src/a.rs"],
        );
        let r = run(dir.path()).unwrap();
        assert!(!rules(&r).contains(&"no-paths-filter"), "{:?}", r.findings);
    }

    #[test]
    fn a_third_party_capability_with_no_paths_is_not_flagged() {
        // musts cannot know what a third-party capability costs, and
        // guessing wrong means noise for every repo shipping an extension.
        let dir = ws(
            "version: 1\nchecks:\n  c:\n    uses: acme/thing\n",
            &["src/a.rs"],
        );
        let r = run(dir.path()).unwrap();
        assert!(!rules(&r).contains(&"no-paths-filter"), "{:?}", r.findings);
    }

    #[test]
    fn a_mav_check_with_no_paths_is_flagged() {
        let dir = ws(
            "version: 1\nchecks:\n  flow:\n    uses: mav/expect\n    with:\n      expectations: [\"e\"]\n      evidence: [\"screenshot\"]\n",
            &["src/a.swift"],
        );
        let r = run(dir.path()).unwrap();
        assert!(rules(&r).contains(&"no-paths-filter"), "{:?}", r.findings);
    }

    #[test]
    fn is_judgment_splits_cost_not_builtin_ness() {
        assert!(is_judgment("agent"));
        assert!(is_judgment("mav/expect"));
        assert!(!is_judgment("cargo/test"));
        assert!(!is_judgment("bazel/build"));
        assert!(!is_judgment("bazel/test"));
        assert!(!is_judgment("acme/thing"));
    }

    /// The case this rule exists for. A repo's `.mustsignore` said
    /// `*.png`, meaning root-level screenshots; unanchored, it also hid
    /// 122 committed snapshot baselines, so the snapshot check could not
    /// see the files it existed to protect. The old message said "check
    /// for a typo" and the glob was fine.
    #[test]
    fn an_empty_match_caused_by_an_ignore_rule_says_so() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("MUSTS.yml"),
            "version: 1\nchecks:\n  snapshot:\n    uses: agent\n    paths: [\"Tests/__Snapshots__/**\"]\n    with:\n      facts: [\"f\"]\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("Tests/__Snapshots__/Foo")).unwrap();
        std::fs::write(dir.path().join("Tests/__Snapshots__/Foo/a.1.png"), "x").unwrap();
        std::fs::write(dir.path().join(".mustsignore"), "*.png\n").unwrap();

        let r = run(dir.path()).unwrap();
        let f = r
            .findings
            .iter()
            .find(|f| f.rule == "glob-matches-nothing")
            .unwrap_or_else(|| panic!("{:?}", r.findings));
        assert!(
            f.message.contains("exist and are excluded"),
            "{}",
            f.message
        );
        assert!(f.message.contains("blind to them"), "{}", f.message);
        assert!(
            f.message.contains("/*.ext` is root-only"),
            "must name the anchoring fix: {}",
            f.message
        );
        assert!(
            !f.message.contains("Check for a typo"),
            "the glob is fine; a typo hint is the wrong steer: {}",
            f.message
        );
    }

    /// A genuinely absent path must keep the typo hint — the new branch
    /// must not swallow the original, correct case.
    #[test]
    fn an_empty_match_with_nothing_on_disk_still_suggests_a_typo() {
        let dir = ws(
            "version: 1\nchecks:\n  c:\n    uses: agent\n    paths: [\"NoSuchDir/**\"]\n    with:\n      facts: [\"f\"]\n",
            &["src/a.swift"],
        );
        let r = run(dir.path()).unwrap();
        let f = r
            .findings
            .iter()
            .find(|f| f.rule == "glob-matches-nothing")
            .unwrap();
        assert!(f.message.contains("Check for a typo"), "{}", f.message);
    }

    /// `paths:` globs are workspace-relative even inside a nested
    /// manifest. Writing them relative to the manifest's own folder
    /// silently matches nothing.
    #[test]
    fn a_manifest_relative_glob_in_a_nested_manifest_is_diagnosed() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("Sub/UI")).unwrap();
        std::fs::write(dir.path().join("Sub/UI/a.swift"), "// x").unwrap();
        std::fs::write(
            dir.path().join("Sub/MUSTS.yml"),
            "version: 1\nchecks:\n  ui:\n    uses: agent\n    paths: [\"UI/*.swift\"]\n    with:\n      facts: [\"f\"]\n",
        )
        .unwrap();

        let r = run(dir.path()).unwrap();
        let f = r
            .findings
            .iter()
            .find(|f| f.rule == "glob-matches-nothing")
            .unwrap_or_else(|| panic!("{:?}", r.findings));
        assert!(
            f.message.contains("`Sub/UI/*.swift` would"),
            "{}",
            f.message
        );
        assert!(f.message.contains("workspace root"), "{}", f.message);
    }

    #[test]
    fn a_suppression_comment_silences_exactly_that_rule() {
        let dir = ws(
            "version: 1\n# musts-lint: allow glob-crosses-directories\nchecks:\n  c:\n    uses: agent\n    paths: [\"src/*.swift\"]\n    with:\n      facts: [\"f\"]\n",
            &["src/a.swift", "src/deep/b.swift"],
        );
        let r = run(dir.path()).unwrap();
        assert!(
            !rules(&r).contains(&"glob-crosses-directories"),
            "{:?}",
            r.findings
        );
    }

    #[test]
    fn suppressing_one_rule_leaves_the_others_reporting() {
        let dir = ws(
            "version: 1\n# musts-lint: allow glob-matches-nothing\nchecks:\n  c:\n    uses: agent\n    paths: [\"src/*.swift\"]\n    with:\n      facts: [\"f\"]\n",
            &["src/a.swift", "src/deep/b.swift"],
        );
        let r = run(dir.path()).unwrap();
        assert!(
            rules(&r).contains(&"glob-crosses-directories"),
            "a suppression must not be a blanket mute: {:?}",
            r.findings
        );
    }

    #[test]
    fn a_suppression_can_list_several_rules() {
        let set =
            suppressed_rules(b"# musts-lint: allow glob-crosses-directories, no-paths-filter\n");
        assert!(set.contains("glob-crosses-directories"));
        assert!(set.contains("no-paths-filter"));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn suppression_parsing_ignores_unrelated_comments_and_yaml() {
        assert!(suppressed_rules(b"# just a comment\nchecks: {}\n").is_empty());
        // Not a comment, so not a directive — a `uses:` value that merely
        // mentions the marker must never silence anything.
        assert!(suppressed_rules(b"uses: musts-lint: allow everything\n").is_empty());
        // Indented comments count; authors indent inside a check block.
        assert!(suppressed_rules(b"    # musts-lint: allow unknown-key\n").contains("unknown-key"));
    }

    #[test]
    fn an_unknown_key_error_can_also_be_suppressed() {
        // Errors gate CI, so being able to opt out matters more, not less.
        let dir = ws(
            "version: 1\n# musts-lint: allow unknown-key\nchecks:\n  c:\n    uses: agent\n    paths: [\"src/**\"]\n    excludes: [\"x\"]\n    with:\n      facts: [\"f\"]\n",
            &["src/a.swift"],
        );
        let r = run(dir.path()).unwrap();
        assert!(!rules(&r).contains(&"unknown-key"), "{:?}", r.findings);
        assert!(!r.has_errors());
    }

    /// A suppression in one manifest must not leak into another.
    #[test]
    fn suppression_is_scoped_to_its_own_manifest() {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src/deep")).unwrap();
        std::fs::create_dir_all(dir.path().join("Sub/src/deep")).unwrap();
        for p in [
            "src/a.swift",
            "src/deep/b.swift",
            "Sub/src/a.swift",
            "Sub/src/deep/b.swift",
        ] {
            std::fs::write(dir.path().join(p), "// x").unwrap();
        }
        std::fs::write(
            dir.path().join("MUSTS.yml"),
            "version: 1\n# musts-lint: allow glob-crosses-directories\nchecks:\n  a:\n    uses: agent\n    paths: [\"src/*.swift\"]\n    with:\n      facts: [\"f\"]\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Sub/MUSTS.yml"),
            "version: 1\nchecks:\n  b:\n    uses: agent\n    paths: [\"Sub/src/*.swift\"]\n    with:\n      facts: [\"f\"]\n",
        )
        .unwrap();

        let r = run(dir.path()).unwrap();
        let crossing: Vec<&Finding> = r
            .findings
            .iter()
            .filter(|f| f.rule == "glob-crosses-directories")
            .collect();
        assert_eq!(
            crossing.len(),
            1,
            "only Sub/ should report: {:?}",
            r.findings
        );
        assert_eq!(crossing[0].manifest, "Sub/MUSTS.yml");
    }
}
