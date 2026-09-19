//! Ledger analytics: what each check has actually cost, and what it has
//! actually caught.
//!
//! The validation loop is only worth its price if the checks in it can
//! fail. A check that reopens on every commit and has never once gone red
//! is pure cost — an agent re-reasons and re-writes prose evidence for a
//! fact that was never in question. Before this command the only way to
//! see that was to grep `.musts/ledger.lock.yaml` by hand.
//!
//! Two data sources, deliberately different in durability:
//!
//! - `.musts/ledger.lock.yaml` — committed, travels with the repo, and is
//!   the authority for *how many distinct states a check has been proven
//!   green for*. One entry per `(check_id, scope_hash)`, appended forever.
//! - `.musts/state.sqlite` — machine-local, wiped by a fresh clone. Adds
//!   submission counts, rejections, and evidence length.
//!
//! Anything sourced only from SQLite is reported as such, because a
//! colleague running this on a fresh clone will legitimately see zeros
//! there while the ledger column is fully populated.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use crate::error::Result;
use crate::state::{lock, Db};

/// Map every check the current tree declares to its `uses:`. Parse
/// failures are skipped rather than fatal: `stats` is a read-only
/// reporting command, and a manifest that is currently mid-edit should
/// not stop you from reading the history of every other check.
pub fn declared_checks(workspace_root: &Path) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for entry in crate::manifest::discover(workspace_root)? {
        let Ok(bytes) = std::fs::read(&entry.abs_path) else {
            continue;
        };
        let Ok(manifest) = crate::manifest::parse(&entry.rel_path, &bytes) else {
            continue;
        };
        let scope = crate::manifest::scope_path_for(&entry.rel_path);
        for (local_id, check) in &manifest.checks {
            out.insert(
                crate::manifest::check_id(&scope, local_id),
                check.uses.clone(),
            );
        }
    }
    Ok(out)
}

/// Per-check analytics. Field docs name the source, since half of these
/// survive a clone and half do not.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CheckStats {
    /// Fully-qualified check id, e.g. `tools/version-policy`.
    pub check_id: String,
    /// Distinct scope hashes this check has been proven green for.
    /// Ledger-sourced, so it survives a clone. This is the headline cost
    /// number: one satisfaction is one round of agent work.
    pub satisfied_scopes: usize,
    /// Times the check went green, came back open, and had to be proven
    /// again — `satisfied_scopes - 1`, since the first satisfaction is
    /// not a reopen. Ledger-sourced.
    pub reopened: usize,
    /// Evidence submissions recorded locally, accepted or not.
    /// SQLite-sourced.
    pub submissions: usize,
    /// Submissions the capability rejected — the check was red, or the
    /// evidence did not prove it green. SQLite-sourced, and only counted
    /// for submissions made by a musts new enough to persist rejections.
    pub red: usize,
    /// Mean characters of agent-written evidence text across recorded
    /// submissions. `None` when nothing was recorded locally.
    /// SQLite-sourced.
    pub mean_evidence_chars: Option<usize>,
    /// Whether a `MUSTS.yml` in the current tree still declares this
    /// check. A `false` here is ledger residue from a deleted check.
    pub declared: bool,
    /// `uses:` of the declaring check, when it is still declared.
    pub capability: Option<String>,
}

impl CheckStats {
    /// A check that keeps reopening but has never gone red: the agent has
    /// paid for it repeatedly and it has never once objected. Not proof
    /// the check is worthless — but it is the only cheap signal that it
    /// might be, and it is what this command exists to surface.
    ///
    /// The threshold is deliberately low. Three reopens with no red is
    /// already worth a glance at the manifest.
    pub fn is_suspect(&self) -> bool {
        self.red == 0 && self.reopened >= SUSPECT_REOPEN_THRESHOLD
    }
}

/// Reopens with zero reds before a check is called out. See
/// [`CheckStats::is_suspect`].
pub const SUSPECT_REOPEN_THRESHOLD: usize = 3;

/// How many suspects the text renderer names before summarising the rest.
/// The table above already lists every check in cost order; this section
/// is a prompt to act, and a 15-line prompt is one nobody reads.
const MAX_LISTED_SUSPECTS: usize = 5;

/// Everything `musts stats` renders.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct StatsReport {
    pub workspace_root: String,
    /// Sorted by cost descending (most-satisfied first), then by id, so
    /// the checks worth looking at are at the top.
    pub checks: Vec<CheckStats>,
    /// Total ledger entries, including any belonging to checks that no
    /// longer exist.
    pub total_satisfied_entries: usize,
    /// False when `.musts/state.sqlite` was absent, which means every
    /// SQLite-sourced column is a zero by default rather than by fact.
    pub local_history_available: bool,
}

/// Build the report. `declared` maps check id → capability for every
/// check the current tree declares; ids absent from it are ledger residue.
///
/// Reads only — no lock is taken, so this never blocks on (or blocks) a
/// concurrent `validate`.
pub fn collect(
    workspace_root: &Path,
    musts_dir: &Path,
    declared: &BTreeMap<String, String>,
) -> Result<StatsReport> {
    let ledger = lock::load(musts_dir)?;

    let mut satisfied_per_check: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    for entry in &ledger.satisfied {
        satisfied_per_check
            .entry(entry.check.clone())
            .or_default()
            .insert(entry.scope_hash.as_str());
    }

    let db_path = musts_dir.join("state.sqlite");
    let local_history_available = db_path.is_file();
    let local = if local_history_available {
        // Opening runs migrations, which is a write. That is fine — the
        // file already exists, and the alternative (a read-only handle
        // that trips over an older schema) fails in a much worse way.
        read_local_history(&crate::state::open(&db_path)?)?
    } else {
        BTreeMap::new()
    };

    // Union of both sources plus the current tree: a check that is
    // declared but never validated should still appear, at zero, because
    // "this check has never run" is itself worth seeing.
    let ids: BTreeSet<&String> = satisfied_per_check
        .keys()
        .chain(local.keys())
        .chain(declared.keys())
        .collect();

    let mut checks: Vec<CheckStats> = ids
        .into_iter()
        .map(|id| {
            let satisfied_scopes = satisfied_per_check.get(id).map_or(0, BTreeSet::len);
            let history = local.get(id);
            CheckStats {
                check_id: id.clone(),
                satisfied_scopes,
                reopened: satisfied_scopes.saturating_sub(1),
                submissions: history.map_or(0, |h| h.submissions),
                red: history.map_or(0, |h| h.red),
                mean_evidence_chars: history.and_then(LocalHistory::mean_evidence_chars),
                declared: declared.contains_key(id),
                capability: declared.get(id).cloned(),
            }
        })
        .collect();

    checks.sort_by(|a, b| {
        b.satisfied_scopes
            .cmp(&a.satisfied_scopes)
            .then_with(|| a.check_id.cmp(&b.check_id))
    });

    Ok(StatsReport {
        workspace_root: workspace_root.display().to_string(),
        checks,
        total_satisfied_entries: ledger.satisfied.len(),
        local_history_available,
    })
}

/// Locally-recorded submission history for one check.
#[derive(Debug, Default, Clone)]
struct LocalHistory {
    submissions: usize,
    red: usize,
    evidence_chars_total: usize,
    /// Submissions that carried any text at all — the divisor for the
    /// mean. A submission with no text would otherwise drag the average
    /// toward zero and make prose-heavy checks look cheap.
    evidence_text_count: usize,
}

impl LocalHistory {
    fn mean_evidence_chars(&self) -> Option<usize> {
        (self.evidence_text_count > 0).then(|| self.evidence_chars_total / self.evidence_text_count)
    }
}

fn read_local_history(db: &Db) -> Result<BTreeMap<String, LocalHistory>> {
    let mut stmt = db
        .conn()
        .prepare("SELECT check_id, accepted, submission_json FROM evidence_records")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)? != 0,
            r.get::<_, String>(2)?,
        ))
    })?;

    let mut out: BTreeMap<String, LocalHistory> = BTreeMap::new();
    for row in rows {
        let (check_id, accepted, submission_json) = row?;
        let entry = out.entry(check_id).or_default();
        entry.submissions += 1;
        if !accepted {
            entry.red += 1;
        }
        if let Some(len) = evidence_text_len(&submission_json) {
            entry.evidence_chars_total += len;
            entry.evidence_text_count += 1;
        }
    }
    Ok(out)
}

/// Length of the agent-written `text` inside a persisted submission.
/// Returns `None` for submissions with no text (or unparseable JSON —
/// a stats command must never fail on a malformed historical row).
fn evidence_text_len(submission_json: &str) -> Option<usize> {
    serde_json::from_str::<serde_json::Value>(submission_json)
        .ok()?
        .get("text")?
        .as_str()
        .map(str::chars)
        .map(Iterator::count)
}

/// Render the human-facing table.
pub fn render_text(report: &StatsReport) -> String {
    let mut out = String::new();

    if report.checks.is_empty() {
        out.push_str("No checks and no ledger history.\n");
        return out;
    }

    out.push_str(&format!(
        "{} check{}, {} satisfied scope{} in the ledger.\n\n",
        report.checks.len(),
        plural(report.checks.len()),
        report.total_satisfied_entries,
        plural(report.total_satisfied_entries),
    ));

    let id_width = report
        .checks
        .iter()
        .map(|c| c.check_id.chars().count())
        .max()
        .unwrap_or(5)
        .max(5);

    out.push_str(&format!(
        "{:<id_width$}  {:>9}  {:>8}  {:>11}  {:>3}  {:>8}  {}\n",
        "CHECK", "SATISFIED", "REOPENED", "SUBMISSIONS", "RED", "EVIDENCE", "USES",
    ));
    for c in &report.checks {
        out.push_str(&format!(
            "{:<id_width$}  {:>9}  {:>8}  {:>11}  {:>3}  {:>8}  {}\n",
            c.check_id,
            c.satisfied_scopes,
            c.reopened,
            c.submissions,
            c.red,
            c.mean_evidence_chars
                .map_or_else(|| "-".to_string(), |n| format!("{n} ch")),
            render_uses(c),
        ));
    }

    let suspects: Vec<&CheckStats> = report.checks.iter().filter(|c| c.is_suspect()).collect();
    if !suspects.is_empty() {
        out.push_str("\nReopened repeatedly, never red:\n");
        for c in suspects.iter().take(MAX_LISTED_SUSPECTS) {
            out.push_str(&format!(
                "  {} — reopened {} time{}, 0 red.\n",
                c.check_id,
                c.reopened,
                plural(c.reopened),
            ));
        }
        // Say what was left out rather than quietly truncating: on a
        // manifest where nearly every check is suspect, "and 10 more"
        // is the finding.
        if suspects.len() > MAX_LISTED_SUSPECTS {
            out.push_str(&format!(
                "  …and {} more (see `--json` for the full list).\n",
                suspects.len() - MAX_LISTED_SUSPECTS
            ));
        }
        out.push_str(
            "\nEach reopen costs a fresh round of validation work. If a check has \
             never objected,\nnarrow it with `paths:`, or move it to a capability \
             that can actually fail.\n",
        );
    }

    if !report.local_history_available {
        out.push_str(
            "\nNo local `.musts/state.sqlite` — SUBMISSIONS, RED and EVIDENCE are \
             machine-local\nand read as 0 on a fresh clone. SATISFIED and REOPENED \
             come from the committed ledger.\n",
        );
    }

    out
}

fn render_uses(c: &CheckStats) -> String {
    match &c.capability {
        Some(uses) => uses.clone(),
        None => "(no longer declared)".to_string(),
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Render the machine-readable form.
pub fn render_json(report: &StatsReport) -> String {
    serde_json::to_string_pretty(report).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn declared(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn write_ledger(musts_dir: &Path, entries: &[(&str, &str)]) {
        let mut l = lock::LedgerLock::default();
        for (check, hash) in entries {
            l.record(*check, *hash);
        }
        lock::save(musts_dir, &l).unwrap();
    }

    fn fresh() -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().unwrap();
        let musts = dir.path().join(".musts");
        std::fs::create_dir_all(&musts).unwrap();
        (dir, musts)
    }

    #[test]
    fn counts_distinct_scopes_per_check() {
        let (dir, musts) = fresh();
        write_ledger(
            &musts,
            &[
                ("a/one", "h1"),
                ("a/one", "h2"),
                ("a/one", "h3"),
                ("b/two", "h1"),
            ],
        );
        let report = collect(dir.path(), &musts, &declared(&[])).unwrap();
        let one = &report.checks[0];
        assert_eq!(one.check_id, "a/one");
        assert_eq!(one.satisfied_scopes, 3);
        assert_eq!(one.reopened, 2, "the first satisfaction is not a reopen");
        assert_eq!(report.checks[1].satisfied_scopes, 1);
        assert_eq!(report.checks[1].reopened, 0);
        assert_eq!(report.total_satisfied_entries, 4);
    }

    #[test]
    fn sorts_most_expensive_first() {
        let (dir, musts) = fresh();
        write_ledger(&musts, &[("zzz", "h1"), ("zzz", "h2"), ("aaa", "h1")]);
        let report = collect(dir.path(), &musts, &declared(&[])).unwrap();
        assert_eq!(report.checks[0].check_id, "zzz");
    }

    #[test]
    fn declared_checks_appear_even_with_no_history() {
        let (dir, musts) = fresh();
        let report = collect(dir.path(), &musts, &declared(&[("new/check", "agent")])).unwrap();
        assert_eq!(report.checks.len(), 1);
        assert_eq!(report.checks[0].satisfied_scopes, 0);
        assert!(report.checks[0].declared);
        assert_eq!(report.checks[0].capability.as_deref(), Some("agent"));
    }

    #[test]
    fn ledger_residue_is_flagged_as_no_longer_declared() {
        let (dir, musts) = fresh();
        write_ledger(&musts, &[("deleted/check", "h1")]);
        let report = collect(dir.path(), &musts, &declared(&[])).unwrap();
        assert!(!report.checks[0].declared);
        assert!(render_text(&report).contains("no longer declared"));
    }

    #[test]
    fn suspect_needs_both_reopens_and_zero_reds() {
        let base = CheckStats {
            check_id: "c".into(),
            satisfied_scopes: 10,
            reopened: 9,
            submissions: 10,
            red: 0,
            mean_evidence_chars: Some(100),
            declared: true,
            capability: Some("agent".into()),
        };
        assert!(base.is_suspect());
        assert!(
            !CheckStats {
                red: 1,
                ..base.clone()
            }
            .is_suspect(),
            "a check that has caught something is never suspect"
        );
        assert!(!CheckStats {
            reopened: 1,
            ..base
        }
        .is_suspect());
    }

    #[test]
    fn text_renderer_calls_out_suspects() {
        let (dir, musts) = fresh();
        let entries: Vec<(String, String)> = (0..8)
            .map(|i| ("tools/version-policy".to_string(), format!("h{i}")))
            .collect();
        let refs: Vec<(&str, &str)> = entries
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        write_ledger(&musts, &refs);
        let report = collect(dir.path(), &musts, &declared(&[])).unwrap();
        let text = render_text(&report);
        assert!(text.contains("Reopened repeatedly, never red"));
        assert!(text.contains("tools/version-policy — reopened 7 times, 0 red"));
    }

    #[test]
    fn missing_local_db_is_reported_not_silently_zero() {
        let (dir, musts) = fresh();
        write_ledger(&musts, &[("a", "h1")]);
        let report = collect(dir.path(), &musts, &declared(&[])).unwrap();
        assert!(!report.local_history_available);
        assert!(render_text(&report).contains("machine-local"));
    }

    #[test]
    fn evidence_text_len_counts_chars_not_bytes() {
        assert_eq!(evidence_text_len(r#"{"text":"héllo"}"#), Some(5));
        assert_eq!(evidence_text_len(r#"{"text":null}"#), None);
        assert_eq!(evidence_text_len("{}"), None);
        assert_eq!(evidence_text_len("not json"), None);
    }

    #[test]
    fn empty_workspace_renders_without_panicking() {
        let (dir, musts) = fresh();
        let report = collect(dir.path(), &musts, &declared(&[])).unwrap();
        assert!(report.checks.is_empty());
        assert!(render_text(&report).contains("No checks"));
    }

    #[test]
    fn suspect_list_is_capped_and_says_what_it_dropped() {
        let (dir, musts) = fresh();
        let mut entries: Vec<(String, String)> = Vec::new();
        for c in 0..8 {
            for h in 0..6 {
                entries.push((format!("check-{c}"), format!("h{c}-{h}")));
            }
        }
        let refs: Vec<(&str, &str)> = entries
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        write_ledger(&musts, &refs);
        let report = collect(dir.path(), &musts, &declared(&[])).unwrap();
        let text = render_text(&report);
        assert!(
            text.contains("…and 3 more"),
            "truncation must be stated:\n{text}"
        );
    }
}
