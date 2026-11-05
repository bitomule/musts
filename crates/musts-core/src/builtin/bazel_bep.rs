//! Reads a Bazel Build Event Protocol JSON file (`--build_event_json_file`)
//! and judges whether the invocation it describes proves a `bazel/test`
//! check, so `musts record` can accept a test run the agent already did.
//!
//! Every decision comes from BEP fields, never from the human log or
//! `statusDetails` (which the proto says "must not be parsed"). Field
//! names follow the proto3 JSON mapping of `build_event_stream.proto`:
//! <https://github.com/bazelbuild/bazel/blob/master/src/main/java/com/google/devtools/build/lib/buildeventstream/proto/build_event_stream.proto>.
//! Glossary: <https://bazel.build/remote/bep-glossary>.
//!
//! What the stream cannot say — which source tree the run saw — comes from
//! the `MUSTS_SCOPES` key that `musts stamp` prints through
//! `--workspace_status_command`; Bazel copies it into the
//! `workspaceStatus` event. Comparing it to the current scope hashes is
//! the caller's job (`crate::record`), since it needs workspace state.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

/// Workspace status key `musts stamp` prints. Not `STABLE_`-prefixed, so
/// a change never invalidates a stamped action or a cached test.
pub const STAMP_KEY: &str = "MUSTS_SCOPES";

/// What one `bazel test` invocation recorded, reduced to the fields the
/// judgement reads.
#[derive(Debug, Default)]
pub struct Invocation {
    pub uuid: String,
    pub command: String,
    pub workspace_directory: String,
    pub start_ms: Option<i64>,
    pub last_message_seen: bool,
    pub finished: bool,
    pub cmd_line: Vec<String>,
    pub requested_patterns: Vec<String>,
    pub patterns_aborted: Option<String>,
    pub suites: BTreeMap<String, Vec<String>>,
    pub test_targets: BTreeSet<String>,
    pub configured: BTreeSet<String>,
    pub summaries: BTreeMap<String, Vec<String>>,
    pub aborted: BTreeMap<String, String>,
    pub stamp: Option<BTreeMap<String, String>>,
}

impl Invocation {
    pub fn short_uuid(&self) -> &str {
        self.uuid.get(..8).unwrap_or(&self.uuid)
    }
}

/// Parse a JSON build event file. `Err` is the reason the file is not a
/// usable build event stream at all.
pub fn parse(bytes: &[u8], shown_path: &str) -> Result<Invocation, String> {
    let not_json = || {
        format!(
            "{shown_path} is not a JSON build event file; write it with \
             --build_event_json_file (the binary --build_event_binary_file is not read)"
        )
    };
    let text = String::from_utf8_lossy(bytes);
    let mut inv = Invocation::default();
    let mut saw_started = false;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // A half-written last line while Bazel is still flushing.
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            inv.last_message_seen = false;
            break;
        };
        if event.get("lastMessage").and_then(Value::as_bool) == Some(true) {
            inv.last_message_seen = true;
        }
        let Some((kind, id)) = event
            .get("id")
            .and_then(Value::as_object)
            .and_then(|o| o.iter().next())
        else {
            continue;
        };
        let aborted_reason = event.get("aborted").map(|a| {
            a.get("reason")
                .and_then(Value::as_str)
                .unwrap_or("UNKNOWN")
                .to_string()
        });
        match kind.as_str() {
            "started" => {
                saw_started = true;
                let s = &event["started"];
                inv.uuid = str_field(s, "uuid");
                inv.command = str_field(s, "command");
                inv.workspace_directory = str_field(s, "workspaceDirectory");
                inv.start_ms = int_field(s, "startTimeMillis").or_else(|| {
                    s.get("startTime")
                        .and_then(Value::as_str)
                        .and_then(rfc3339_ms)
                });
            }
            "optionsParsed" => {
                inv.cmd_line = strings(&event["optionsParsed"]["cmdLine"]);
            }
            "pattern" => {
                inv.requested_patterns = strings(&id["pattern"]);
                if let Some(reason) = aborted_reason {
                    inv.patterns_aborted = Some(reason);
                }
                if let Some(expansions) = event["expanded"]["testSuiteExpansions"].as_array() {
                    for e in expansions {
                        let suite = normalise_label(&str_field(e, "suiteLabel"));
                        let tests = match &e["testLabels"] {
                            Value::String(s) => vec![normalise_label(s)],
                            other => strings(other).iter().map(|l| normalise_label(l)).collect(),
                        };
                        inv.suites.insert(suite, tests);
                    }
                }
            }
            "targetConfigured" => {
                let label = normalise_label(&str_field(id, "label"));
                let kind = event["configured"]["targetKind"].as_str().unwrap_or("");
                if kind.ends_with("_test rule") {
                    inv.test_targets.insert(label.clone());
                }
                inv.configured.insert(label);
            }
            "targetCompleted" => {
                if let Some(reason) = aborted_reason {
                    inv.aborted
                        .insert(normalise_label(&str_field(id, "label")), reason);
                }
            }
            "testSummary" => {
                let label = normalise_label(&str_field(id, "label"));
                let status = event["testSummary"]["overallStatus"]
                    .as_str()
                    .unwrap_or("NO_STATUS")
                    .to_string();
                inv.summaries.entry(label).or_default().push(status);
            }
            "workspaceStatus" => {
                if let Some(items) = event["workspaceStatus"]["item"].as_array() {
                    for item in items {
                        if item["key"].as_str() == Some(STAMP_KEY) {
                            inv.stamp = item["value"]
                                .as_str()
                                .and_then(|v| serde_json::from_str(v).ok());
                        }
                    }
                }
            }
            "buildFinished" => inv.finished = true,
            _ => {}
        }
    }
    if !saw_started {
        return Err(not_json());
    }
    Ok(inv)
}

/// Reasons that disqualify the whole invocation, whatever the check.
pub fn invocation_problem(inv: &Invocation, workspace_root: &Path) -> Option<String> {
    if !inv.last_message_seen || !inv.finished {
        return Some(
            "the build event file is incomplete: bazel was still running or was interrupted".into(),
        );
    }
    if inv.command != "test" {
        return Some(format!(
            "the invocation was \"bazel {}\", not \"bazel test\"",
            inv.command
        ));
    }
    if !same_dir(Path::new(&inv.workspace_directory), workspace_root) {
        return Some(format!(
            "the invocation ran in {}, not in this workspace ({})",
            inv.workspace_directory,
            workspace_root.display()
        ));
    }
    if let Some(filter) = test_filter(&inv.cmd_line) {
        return Some(format!(
            "--test_filter={filter} ran only part of each test; a filtered run does not prove \
             the check"
        ));
    }
    if let Some(reason) = &inv.patterns_aborted {
        return Some(format!(
            "the invocation's target patterns failed to load ({reason})"
        ));
    }
    None
}

/// Judge one check's `targets:` against the invocation. `Ok` carries how
/// many tests passed; `Err` the first target that is not proven.
pub fn targets_proven(inv: &Invocation, targets: &[String]) -> Result<usize, String> {
    let mut positives = Vec::new();
    let mut exclusions = Vec::new();
    for raw in targets {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let (negative, body) = match raw.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, raw),
        };
        let pattern = Pattern::parse(body)
            .ok_or_else(|| format!("{raw} is a target pattern musts cannot judge from BEP"))?;
        if negative {
            exclusions.push(pattern);
        } else {
            positives.push((raw, pattern));
        }
    }

    let mut passed: BTreeSet<&str> = BTreeSet::new();
    for (raw, pattern) in &positives {
        match pattern {
            Pattern::Label(label) => {
                let tests = match inv.suites.get(label) {
                    Some(tests) => tests.clone(),
                    None => vec![label.clone()],
                };
                for test in &tests {
                    if exclusions.iter().any(|e| e.contains_label(test)) {
                        continue;
                    }
                    require_passed(inv, test)?;
                    passed.insert(inv.summaries.get_key_value(test).unwrap().0);
                }
            }
            Pattern::Recursive(_) | Pattern::Package(_) => {
                require_requested(inv, raw, pattern, &exclusions)?;
                let mut under = 0;
                for test in inv.test_targets.iter().chain(inv.summaries.keys()) {
                    if !pattern.contains_label(test)
                        || exclusions.iter().any(|e| e.contains_label(test))
                        || inv.aborted.get(test).map(String::as_str) == Some("SKIPPED")
                    {
                        continue;
                    }
                    require_passed(inv, test)?;
                    if passed.insert(test) {
                        under += 1;
                    }
                }
                if let Some((label, reason)) = inv
                    .aborted
                    .iter()
                    .find(|(l, r)| pattern.contains_label(l) && r.as_str() != "SKIPPED")
                {
                    return Err(format!("{label} was aborted ({reason})"));
                }
                if under == 0 {
                    return Err(format!("no test under {raw} passed in the invocation"));
                }
            }
        }
    }
    Ok(passed.len())
}

fn require_passed(inv: &Invocation, label: &str) -> Result<(), String> {
    let Some(statuses) = inv.summaries.get(label) else {
        if let Some(reason) = inv.aborted.get(label) {
            return Err(format!("{label} was aborted ({reason})"));
        }
        if inv.configured.contains(label) {
            return Err(if inv.test_targets.contains(label) {
                format!("{label} was built but not run (excluded by a test filter)")
            } else {
                format!("{label} is not a test target")
            });
        }
        return Err(format!(
            "{label} was not part of the invocation (requested: {})",
            inv.requested_patterns.join(" ")
        ));
    };
    match statuses.iter().find(|s| s.as_str() != "PASSED") {
        None => Ok(()),
        Some(s) if s == "FLAKY" => Err(format!(
            "{label} was FLAKY (passed only on a retry); a flaky pass does not prove the check"
        )),
        Some(s) => Err(format!("{label} {s}")),
    }
}

fn require_requested(
    inv: &Invocation,
    raw: &str,
    pattern: &Pattern,
    exclusions: &[Pattern],
) -> Result<(), String> {
    let mut covered = false;
    for requested in &inv.requested_patterns {
        let requested = requested.trim();
        if let Some(negative) = requested.strip_prefix('-') {
            let Some(n) = Pattern::parse(negative) else {
                continue;
            };
            if n.overlaps(pattern) && !exclusions.iter().any(|e| e.covers(&n)) {
                return Err(format!("the invocation excluded {requested}, inside {raw}"));
            }
        } else if Pattern::parse(requested).is_some_and(|q| q.covers(pattern)) {
            covered = true;
        }
    }
    if covered {
        Ok(())
    } else {
        Err(format!(
            "{raw} was not requested; the invocation requested {}",
            inv.requested_patterns.join(" ")
        ))
    }
}

/// A main-repository target pattern, normalised.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pattern {
    /// `//pkg:name`.
    Label(String),
    /// `//pkg/...` (any target suffix); `""` is `//...`.
    Recursive(String),
    /// `//pkg:all`, `//pkg:*`, `//pkg:all-targets`.
    Package(String),
}

impl Pattern {
    fn parse(raw: &str) -> Option<Self> {
        let rest = strip_main_repo(raw.trim())?.strip_prefix("//")?;
        let (pkg, name) = match rest.split_once(':') {
            Some((pkg, name)) => (pkg, Some(name)),
            None => (rest, None),
        };
        let is_wild_name = |n: &str| matches!(n, "all" | "*" | "all-targets");
        if pkg == "..." || pkg.ends_with("/...") {
            if name.is_some_and(|n| !is_wild_name(n)) {
                return None;
            }
            let base = pkg.trim_end_matches("...").trim_end_matches('/');
            return Some(Self::Recursive(base.to_string()));
        }
        if pkg.contains("...") {
            return None;
        }
        match name {
            Some(n) if is_wild_name(n) => Some(Self::Package(pkg.to_string())),
            Some(n) => Some(Self::Label(format!("//{pkg}:{n}"))),
            None => {
                let last = pkg.rsplit('/').next().filter(|s| !s.is_empty())?;
                Some(Self::Label(format!("//{pkg}:{last}")))
            }
        }
    }

    fn contains_label(&self, label: &str) -> bool {
        let Some(pkg) = label
            .strip_prefix("//")
            .and_then(|r| r.split_once(':'))
            .map(|(p, _)| p)
        else {
            return false;
        };
        match self {
            Self::Label(l) => l == label,
            Self::Package(p) => p == pkg,
            Self::Recursive(r) => package_under(pkg, r),
        }
    }

    /// Every target `other` names is also named by `self`.
    fn covers(&self, other: &Pattern) -> bool {
        match (self, other) {
            (Self::Recursive(r), Self::Recursive(o)) | (Self::Recursive(r), Self::Package(o)) => {
                package_under(o, r)
            }
            (Self::Package(p), Self::Package(o)) => p == o,
            (_, Self::Label(l)) => self.contains_label(l),
            _ => false,
        }
    }

    fn overlaps(&self, other: &Pattern) -> bool {
        self.covers(other) || other.covers(self)
    }
}

fn package_under(pkg: &str, root: &str) -> bool {
    root.is_empty() || pkg == root || pkg.starts_with(&format!("{root}/"))
}

fn strip_main_repo(raw: &str) -> Option<&str> {
    if let Some(rest) = raw.strip_prefix("@@") {
        return rest.starts_with("//").then_some(rest);
    }
    if let Some(rest) = raw.strip_prefix('@') {
        return rest.starts_with("//").then_some(rest);
    }
    Some(raw)
}

/// `//pkg:name` with the main-repo prefix and the `//pkg` shorthand
/// folded away, so manifest targets and BEP labels compare equal.
pub fn normalise_label(raw: &str) -> String {
    match Pattern::parse(raw) {
        Some(Pattern::Label(l)) => l,
        _ => raw.trim().to_string(),
    }
}

fn test_filter(cmd_line: &[String]) -> Option<String> {
    let mut args = cmd_line.iter();
    while let Some(arg) = args.next() {
        let value = if let Some(v) = arg.strip_prefix("--test_filter=") {
            Some(v.to_string())
        } else if arg == "--test_filter" {
            args.next().cloned()
        } else {
            None
        };
        if let Some(v) = value.filter(|v| !v.is_empty()) {
            return Some(v);
        }
    }
    None
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn int_field(v: &Value, key: &str) -> Option<i64> {
    match v.get(key)? {
        Value::String(s) => s.parse().ok(),
        Value::Number(n) => n.as_i64(),
        _ => None,
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn rfc3339_ms(s: &str) -> Option<i64> {
    let t = time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()?;
    i64::try_from(t.unix_timestamp_nanos() / 1_000_000).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `bazel test //a/... --keep_going --test_tag_filters=-slow` on Bazel
    /// 8.4.2, trimmed to the events this module reads: `ok_test` and
    /// `b:deep_test` pass, `fail_test` fails, `flaky_test` passes on its
    /// second attempt, `slow_test` is filtered out, `manual_test` is not
    /// expanded by the wildcard.
    const REAL: &str = include_str!("testdata/bep_bazel_8_4_2.json");

    fn real() -> Invocation {
        parse(REAL.as_bytes(), "bep.json").unwrap()
    }

    fn targets(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn judge(inv: &Invocation, list: &[&str]) -> Result<usize, String> {
        targets_proven(inv, &targets(list))
    }

    /// A synthetic stream in the same shapes as the real capture.
    struct Stream {
        lines: Vec<serde_json::Value>,
    }

    impl Stream {
        fn test(patterns: &[&str]) -> Self {
            let mut s = Self { lines: Vec::new() };
            s.lines.push(json!({"id":{"started":{}},"started":{"uuid":"95e65c67-f4be-40ba-b9ef-1f74acead8ce","startTimeMillis":"1793844104395","command":"test","workspaceDirectory":"/ws"}}));
            s.lines.push(
                json!({"id":{"optionsParsed":{}},"optionsParsed":{"cmdLine":["--keep_going"]}}),
            );
            s.lines
                .push(json!({"id":{"pattern":{"pattern":patterns}},"expanded":{}}));
            s.lines.push(json!({"id":{"workspaceStatus":{}},"workspaceStatus":{"item":[{"key":"MUSTS_SCOPES","value":"{\"root/t\":\"abc\"}"}]}}));
            s
        }

        fn passed(mut self, label: &str) -> Self {
            self.configured(label, "sh_test rule");
            self.summary(label, "PASSED");
            self
        }

        fn status(mut self, label: &str, status: &str) -> Self {
            self.configured(label, "sh_test rule");
            self.summary(label, status);
            self
        }

        fn configured(&mut self, label: &str, kind: &str) {
            self.lines.push(
                json!({"id":{"targetConfigured":{"label":label}},"configured":{"targetKind":kind}}),
            );
        }

        fn summary(&mut self, label: &str, status: &str) {
            self.lines.push(json!({"id":{"testSummary":{"label":label,"configuration":{"id":"c1"}}},"testSummary":{"totalRunCount":1,"overallStatus":status,"totalNumCached":1}}));
        }

        fn aborted(mut self, label: &str, kind: &str, reason: &str) -> Self {
            self.configured(label, kind);
            self.lines.push(json!({"id":{"targetCompleted":{"label":label,"configuration":{"id":"c1"}}},"aborted":{"reason":reason}}));
            self
        }

        fn line(mut self, v: serde_json::Value) -> Self {
            self.lines.push(v);
            self
        }

        fn finish(mut self) -> Invocation {
            self.lines.push(
                json!({"id":{"buildFinished":{}},"finished":{"exitCode":{"name":"SUCCESS"}}}),
            );
            self.lines
                .push(json!({"id":{"buildMetrics":{}},"lastMessage":true,"buildMetrics":{}}));
            self.parse()
        }

        fn parse(&self) -> Invocation {
            let text: Vec<String> = self.lines.iter().map(|l| l.to_string()).collect();
            parse(text.join("\n").as_bytes(), "bep.json").unwrap()
        }
    }

    #[test]
    fn real_capture_parses_every_field_the_judge_reads() {
        let inv = real();
        assert_eq!(inv.command, "test");
        assert_eq!(inv.workspace_directory, "/ws");
        assert_eq!(inv.start_ms, Some(1_793_844_091_644));
        assert_eq!(inv.requested_patterns, ["//a/..."]);
        assert_eq!(inv.summaries["//a:flaky_test"], ["FLAKY"]);
        assert_eq!(inv.summaries["//a:fail_test"], ["FAILED"]);
        assert!(inv.test_targets.contains("//a:slow_test"));
        assert!(!inv.summaries.contains_key("//a:slow_test"));
        assert_eq!(inv.suites["//a:suite"], ["//a:ok_test", "//a:slow_test"]);
        assert_eq!(inv.stamp.as_ref().unwrap()["root/t"], "abc");
        assert!(inv.last_message_seen && inv.finished);
        assert_eq!(invocation_problem(&inv, Path::new("/ws")), None);
    }

    #[test]
    fn accepts_passed_labels_from_the_real_capture() {
        assert_eq!(judge(&real(), &["//a:ok_test", "//a/b:deep_test"]), Ok(2));
    }

    #[test]
    fn accepts_a_pattern_whose_tests_all_passed() {
        assert_eq!(judge(&real(), &["//a/b/..."]), Ok(1));
    }

    #[test]
    fn accepts_a_suite_whose_tests_all_passed_and_cached_passes() {
        let inv = Stream::test(&["//a:suite"])
            .line(json!({"id":{"pattern":{"pattern":["//a:suite"]}},"expanded":{"testSuiteExpansions":[{"suiteLabel":"//a:suite","testLabels":["//a:one_test","//a:two_test"]}]}}))
            .passed("//a:one_test")
            .passed("//a:two_test")
            .finish();
        assert_eq!(judge(&inv, &["//a:suite"]), Ok(2));
    }

    #[test]
    fn main_repo_prefixes_and_package_shorthand_are_the_same_label() {
        let inv = Stream::test(&["//pkg"]).passed("@@//pkg:pkg").finish();
        assert_eq!(judge(&inv, &["//pkg"]), Ok(1));
        assert_eq!(judge(&inv, &["@//pkg:pkg"]), Ok(1));
    }

    #[test]
    fn rejects_a_failed_test() {
        assert_eq!(
            judge(&real(), &["//a:fail_test"]),
            Err("//a:fail_test FAILED".into())
        );
    }

    #[test]
    fn rejects_a_timed_out_test() {
        let inv = Stream::test(&["//a:t"]).status("//a:t", "TIMEOUT").finish();
        assert_eq!(judge(&inv, &["//a:t"]), Err("//a:t TIMEOUT".into()));
    }

    #[test]
    fn rejects_a_flaky_pass() {
        let err = judge(&real(), &["//a:flaky_test"]).unwrap_err();
        assert!(err.contains("//a:flaky_test was FLAKY"), "{err}");
    }

    #[test]
    fn rejects_a_test_excluded_by_a_filter() {
        assert_eq!(
            judge(&real(), &["//a:slow_test"]),
            Err("//a:slow_test was built but not run (excluded by a test filter)".into())
        );
    }

    #[test]
    fn rejects_a_suite_when_one_of_its_tests_was_filtered() {
        let err = judge(&real(), &["//a:suite"]).unwrap_err();
        assert!(
            err.starts_with("//a:slow_test was built but not run"),
            "{err}"
        );
    }

    #[test]
    fn rejects_a_target_missing_from_the_invocation() {
        assert_eq!(
            judge(&real(), &["//tools:licenses_test"]),
            Err("//tools:licenses_test was not part of the invocation (requested: //a/...)".into())
        );
    }

    #[test]
    fn rejects_a_pattern_the_invocation_did_not_request() {
        assert_eq!(
            judge(&real(), &["//b/..."]),
            Err("//b/... was not requested; the invocation requested //a/...".into())
        );
    }

    #[test]
    fn rejects_a_narrower_request_than_the_check_pattern() {
        let inv = Stream::test(&["//a/b/..."])
            .passed("//a/b:deep_test")
            .finish();
        let err = judge(&inv, &["//a/..."]).unwrap_err();
        assert!(err.starts_with("//a/... was not requested"), "{err}");
    }

    #[test]
    fn rejects_a_pattern_with_any_failing_test_under_it() {
        assert_eq!(
            judge(&real(), &["//a/..."]),
            Err("//a:fail_test FAILED".into())
        );
    }

    #[test]
    fn rejects_an_invocation_that_excluded_part_of_the_pattern() {
        let inv = Stream::test(&["//a/...", "-//a/legacy/..."])
            .passed("//a:t")
            .finish();
        assert_eq!(
            judge(&inv, &["//a/..."]),
            Err("the invocation excluded -//a/legacy/..., inside //a/...".into())
        );
    }

    #[test]
    fn accepts_an_exclusion_the_check_itself_declares() {
        let inv = Stream::test(&["//a/...", "-//a/legacy/..."])
            .passed("//a:t")
            .finish();
        assert_eq!(judge(&inv, &["//a/...", "-//a/legacy/..."]), Ok(1));
    }

    #[test]
    fn rejects_an_aborted_test_and_ignores_incompatible_ones() {
        let inv = Stream::test(&["//a/..."])
            .passed("//a:t")
            .aborted("//a:mac_only_test", "sh_test rule", "SKIPPED")
            .finish();
        assert_eq!(judge(&inv, &["//a/..."]), Ok(1));
        let inv = Stream::test(&["//a/..."])
            .passed("//a:t")
            .aborted("//a:broken_test", "sh_test rule", "ANALYSIS_FAILURE")
            .finish();
        assert_eq!(
            judge(&inv, &["//a/..."]),
            Err("//a:broken_test was aborted (ANALYSIS_FAILURE)".into())
        );
    }

    #[test]
    fn rejects_a_pattern_whose_library_failed_to_build() {
        let inv = Stream::test(&["//a/..."])
            .passed("//a:t")
            .aborted("//a:lib", "rust_library rule", "ANALYSIS_FAILURE")
            .finish();
        assert_eq!(
            judge(&inv, &["//a/..."]),
            Err("//a:lib was aborted (ANALYSIS_FAILURE)".into())
        );
    }

    #[test]
    fn rejects_a_failed_test_whose_rule_kind_does_not_end_in_test() {
        let mut s = Stream::test(&["//a/..."]).passed("//a:t");
        s.configured("//a:bundle", "_ios_internal_unit_test_bundle rule");
        s.summary("//a:bundle", "FAILED");
        let inv = s.finish();
        assert_eq!(judge(&inv, &["//a/..."]), Err("//a:bundle FAILED".into()));
    }

    #[test]
    fn rejects_a_pattern_with_no_test_under_it() {
        let inv = Stream::test(&["//a/..."]).finish();
        assert_eq!(
            judge(&inv, &["//a/..."]),
            Err("no test under //a/... passed in the invocation".into())
        );
    }

    #[test]
    fn rejects_a_binary_file() {
        let err = parse(&[0x0a, 0x8f, 0xff, 0x01], "bep.bin").unwrap_err();
        assert!(err.contains("is not a JSON build event file"), "{err}");
    }

    #[test]
    fn rejects_a_human_log() {
        let err = parse(
            b"INFO: Build completed successfully\n//a:t PASSED\n",
            "test.log",
        )
        .unwrap_err();
        assert!(err.contains("is not a JSON build event file"), "{err}");
    }

    #[test]
    fn rejects_an_incomplete_stream() {
        let mut s = Stream::test(&["//a:t"]).passed("//a:t");
        s.lines
            .push(json!({"id":{"buildFinished":{}},"finished":{}}));
        let inv = s.parse();
        assert_eq!(
            invocation_problem(&inv, Path::new("/ws")),
            Some(
                "the build event file is incomplete: bazel was still running or was interrupted"
                    .into()
            )
        );
    }

    #[test]
    fn rejects_a_build_invocation() {
        let mut inv = Stream::test(&["//a:t"]).finish();
        inv.command = "build".into();
        assert_eq!(
            invocation_problem(&inv, Path::new("/ws")),
            Some("the invocation was \"bazel build\", not \"bazel test\"".into())
        );
    }

    #[test]
    fn rejects_an_invocation_from_another_workspace() {
        let inv = Stream::test(&["//a:t"]).finish();
        let err = invocation_problem(&inv, Path::new("/elsewhere")).unwrap();
        assert!(
            err.starts_with("the invocation ran in /ws, not in this workspace"),
            "{err}"
        );
    }

    #[test]
    fn rejects_a_test_filter_from_the_command_line_or_an_rc_file() {
        for cmd_line in [
            json!(["--test_filter=foo"]),
            json!(["--test_filter", "foo"]),
        ] {
            let inv = Stream::test(&["//a:t"])
                .line(json!({"id":{"optionsParsed":{}},"optionsParsed":{"cmdLine":cmd_line}}))
                .finish();
            assert_eq!(
                invocation_problem(&inv, Path::new("/ws")),
                Some("--test_filter=foo ran only part of each test; a filtered run does not prove the check".into())
            );
        }
    }

    #[test]
    fn a_missing_stamp_reads_as_none() {
        let mut s = Stream::test(&["//a:t"]);
        s.lines.retain(|l| l["id"].get("workspaceStatus").is_none());
        let inv = Stream { lines: s.lines }.finish();
        assert!(inv.stamp.is_none());
    }
}
