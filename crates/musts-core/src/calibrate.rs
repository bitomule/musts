//! Calibration: does a `uses: jev` question decide anything, and does it
//! decide the right way?
//!
//! Today a badly written judgment check is invisible until it fires in
//! production — or, worse, until it never fires and nobody notices. This
//! module is the pure half of the answer: it takes the probabilities a
//! question returned on real file states from the repo's own history,
//! plus its two control results, and says which of six things the
//! question is.
//!
//! The control is not a nicety, and the number that proves it was
//! measured on this repo: `self_comparing` answered 0.04, 0.04, 0.07,
//! 0.08, 0.19, 0.19 on six real historic file states — it fired on none
//! of them, and its maximum never left the confident-no band. A question
//! that cannot fire at all produces exactly those numbers too. History
//! alone cannot tell "this repo is clean" from "this question is
//! broken"; only a planted violation can. Hence [`Verdict::Uncontrolled`]
//! is a verdict and not a footnote.
//!
//! Nothing here writes to `MUSTS.yml`. A calibration that disables a
//! check on its own is the silent green this project exists to remove:
//! the report names the check, the number and the edit, and a person
//! makes it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Fire rate at or above which a question is noise rather than signal.
///
/// Borrowed from `abide`'s `NOISY_FIRE_RATE` and **not independently
/// validated**: it is a starting point, recorded in every report so a
/// later run can argue with it.
pub const NOISY_FIRE_RATE: f64 = 0.6;

/// Below this a "no" is confident. A question whose highest answer over
/// real history never reaches it never came close to firing.
///
/// Borrowed from `abide`'s `CONFIDENT_NO`, same caveat.
pub const CONFIDENT_NO: f64 = 0.25;

/// At or above this a question came close to firing even if it did not.
/// A question that reaches it on real history discriminated something,
/// so it is not mute.
///
/// Borrowed from `abide`'s `CLEAR_YES`, same caveat.
pub const CLEAR_YES: f64 = 0.7;

/// Fewer real samples than this and the history axis says nothing; the
/// controls still do.
pub const MIN_SAMPLES: usize = 5;

/// What a single `musts-jev` call answered about one file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sample {
    /// Repo-relative path of the file that was judged.
    pub file: String,
    /// Commit the file state was taken from.
    pub commit: String,
    /// Probability that the question's `true` branch holds, verbatim
    /// from jev.
    pub p: f64,
    /// `ok`, `FAIL` or `UNSURE`, as `musts-jev` decided against the
    /// check's `expect:`.
    pub verdict: String,
}

impl Sample {
    /// Probability that this file **violates** the rule, which is `p`
    /// only when the check expects `no`.
    ///
    /// Getting this backwards silently inverts every verdict below, so
    /// it is one function and not an inline expression.
    fn violation_p(&self, expect: Expect) -> f64 {
        match expect {
            Expect::No => self.p,
            Expect::Yes => 1.0 - self.p,
        }
    }

    fn fired(&self) -> bool {
        self.verdict == "FAIL"
    }
}

/// Which answer the check treats as healthy — the manifest's `expect:`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Expect {
    Yes,
    No,
}

impl Expect {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "yes" => Some(Self::Yes),
            "no" => Some(Self::No),
            _ => None,
        }
    }
}

/// The result of running the question against its two planted files.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Controls {
    /// A file that really does break the rule. The question must fire.
    pub violating: ControlResult,
    /// A near-miss that does not break it. The question must stay quiet.
    ///
    /// Near-miss is the whole requirement: a clean control that shares
    /// nothing with the violating one proves only that the question can
    /// tell source code from an empty file.
    pub clean: ControlResult,
}

impl Controls {
    /// Both controls answered the way they were planted.
    pub fn passed(&self) -> bool {
        self.violating.fired && !self.clean.fired
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ControlResult {
    pub file: String,
    pub p: f64,
    pub verdict: String,
    /// Whether the question fired on this file.
    pub fired: bool,
}

/// What calibration concluded about one question. Ordered by
/// precedence: the first that applies is the one reported.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// A control answered the wrong way. Every number below it is
    /// meaningless, and this is the case that today only surfaces in
    /// production.
    Broken,
    /// No controls declared, so "the repo is clean" and "the question
    /// cannot fire" are indistinguishable. Not a pass.
    Uncontrolled,
    /// Too few real samples to say anything about history. The controls
    /// still stand on their own.
    Thin,
    /// Fires on most of real history. A check that fires on everything
    /// decides nothing.
    Noisy,
    /// Controls pass, and real history never came near firing. **Not a
    /// defect and not a reason to disable it**: there is nothing to find
    /// here yet.
    Mute,
    /// Controls pass, it fires on a minority, and its answers reach both
    /// ends.
    Decisive,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Self::Broken => "broken",
            Self::Uncontrolled => "uncontrolled",
            Self::Thin => "thin",
            Self::Noisy => "noisy",
            Self::Mute => "mute",
            Self::Decisive => "decisive",
        }
    }

    /// The sentence the report prints. `mute` carries David's words on
    /// purpose: it is the one verdict readers reliably misread as a
    /// failure.
    pub fn explain(self) -> &'static str {
        match self {
            Self::Broken => "a control answered the wrong way — this question does not work",
            Self::Uncontrolled => {
                "no control declared, so a clean repo and a broken question look identical here"
            }
            Self::Thin => "too little history to judge; the controls are all this rests on",
            Self::Noisy => "fires on most of real history, so it decides nothing",
            Self::Mute => "nothing to find here yet — the question works, the repo is clean",
            Self::Decisive => "fires on a minority of real history and its controls hold",
        }
    }

    /// Whether a person is being asked to change something.
    pub fn needs_action(self) -> bool {
        matches!(self, Self::Broken | Self::Noisy | Self::Uncontrolled)
    }
}

/// Everything calibration learned about one question.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuestionReport {
    /// Fully-qualified check id from the manifest.
    pub check_id: String,
    /// The question asked, i.e. the check's `ask:`.
    pub ask: String,
    pub expect: Expect,
    pub verdict: Verdict,
    /// Commit that introduced the question set, and the boundary every
    /// sample is older than. `None` when it could not be determined —
    /// which is reported, never silently ignored.
    pub introduced_at: Option<String>,
    /// How many samples were asked for, and how many were found. When
    /// these differ the report says so rather than reaching for newer
    /// commits.
    pub samples_wanted: usize,
    pub samples: Vec<Sample>,
    pub controls: Option<Controls>,
    pub median_p: f64,
    pub min_p: f64,
    pub max_p: f64,
    pub fired: usize,
    /// The model that answered, as reported by jev.
    pub model: Option<String>,
    /// The one-line edit a person would make, for the verdicts that ask
    /// for one.
    pub suggested_edit: Option<String>,
}

/// Decide what a question is from its samples and its controls.
///
/// Precedence is deliberate: a failing control outranks every history
/// number, and a missing control outranks a thin sample, because the
/// control is what makes the history readable at all.
pub fn classify(samples: &[Sample], controls: Option<&Controls>, expect: Expect) -> Verdict {
    match controls {
        Some(c) if !c.passed() => return Verdict::Broken,
        None => return Verdict::Uncontrolled,
        Some(_) => {}
    }
    if samples.len() < MIN_SAMPLES {
        return Verdict::Thin;
    }
    let fired = samples.iter().filter(|s| s.fired()).count();
    #[allow(clippy::cast_precision_loss)]
    if fired as f64 / samples.len() as f64 >= NOISY_FIRE_RATE {
        return Verdict::Noisy;
    }
    let max = samples
        .iter()
        .map(|s| s.violation_p(expect))
        .fold(f64::MIN, f64::max);
    // Fired on nothing and never came close: there is nothing here to
    // find. A question that reached `CLEAR_YES` without firing still
    // separated one file from the rest, which is discrimination.
    if fired == 0 && max < CLEAR_YES {
        return Verdict::Mute;
    }
    Verdict::Decisive
}

/// Median of `values`, or `0.0` when empty. Even-length takes the mean
/// of the two middle values.
fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    }
}

/// Everything one calibration run produced for one question, before it
/// is summarised.
pub struct Run {
    pub check_id: String,
    pub ask: String,
    pub expect: Expect,
    /// Commit the question first existed in; every sample is older.
    pub introduced_at: Option<String>,
    pub samples_wanted: usize,
    pub samples: Vec<Sample>,
    pub controls: Option<Controls>,
    pub model: Option<String>,
}

/// Assemble one question's report, computing the summary statistics and
/// the edit a person would make.
#[must_use]
pub fn summarise(run: Run) -> QuestionReport {
    let Run {
        check_id,
        ask,
        expect,
        introduced_at,
        samples_wanted,
        samples,
        controls,
        model,
    } = run;
    let verdict = classify(&samples, controls.as_ref(), expect);
    let violation: Vec<f64> = samples.iter().map(|s| s.violation_p(expect)).collect();
    let fired = samples.iter().filter(|s| s.fired()).count();
    let suggested_edit = match verdict {
        Verdict::Broken => Some(format!(
            "rewrite the question `{ask}` or its controls — do not ship it as a tripwire"
        )),
        Verdict::Noisy => Some(format!(
            "set `mode: shadow` on check `{check_id}` until `{ask}` stops firing on most files"
        )),
        Verdict::Uncontrolled => Some(format!(
            "add `control: {{violating: <path>, clean: <path>}}` to check `{check_id}`"
        )),
        Verdict::Thin | Verdict::Mute | Verdict::Decisive => None,
    };
    QuestionReport {
        check_id,
        ask,
        expect,
        verdict,
        introduced_at,
        samples_wanted,
        median_p: median(&violation),
        min_p: violation.iter().copied().fold(f64::MAX, f64::min).min(1.0),
        max_p: violation.iter().copied().fold(f64::MIN, f64::max).max(0.0),
        fired,
        samples,
        controls,
        model,
        suggested_edit,
    }
}

/// The committed record, one file per workspace.
///
/// It is committed on purpose: a reviewer looking at a judgment check
/// should be able to see what it was calibrated against and when,
/// without running anything.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CalibrationRecord {
    pub version: u32,
    /// Seconds since the Unix epoch. Not a formatted date: musts has no
    /// date dependency, and a hand-rolled one would be wrong in some
    /// timezone nobody tests in.
    pub at_unix: u64,
    /// `HEAD` when the run was made, so a reviewer can tell how stale
    /// the record is.
    pub head: String,
    pub questions: BTreeMap<String, QuestionReport>,
}

impl CalibrationRecord {
    pub fn new(at_unix: u64, head: String, reports: Vec<QuestionReport>) -> Self {
        Self {
            version: 1,
            at_unix,
            head,
            questions: reports
                .into_iter()
                .map(|r| (r.check_id.clone(), r))
                .collect(),
        }
    }
}

/// Human-readable report. Every question gets its verdict, its numbers
/// and — when a person has to act — the exact edit.
#[must_use]
pub fn render_text(record: &CalibrationRecord) -> String {
    let mut out = String::new();
    out.push_str("Musts calibration\n\n");
    if record.questions.is_empty() {
        out.push_str("No `uses: jev` checks declared. Nothing to calibrate.\n");
        return out;
    }
    for report in record.questions.values() {
        out.push_str(&render_question(report));
        out.push('\n');
    }
    let action: Vec<&QuestionReport> = record
        .questions
        .values()
        .filter(|r| r.verdict.needs_action())
        .collect();
    if action.is_empty() {
        out.push_str("Nothing to change. This report edited no manifest.\n");
    } else {
        out.push_str("Edits for you to make — calibration changed no manifest:\n");
        for report in action {
            if let Some(edit) = &report.suggested_edit {
                out.push_str(&format!("  {}: {}\n", report.check_id, edit));
            }
        }
    }
    out
}

fn render_question(r: &QuestionReport) -> String {
    let mut out = format!(
        "{}  [{}]  {}\n",
        r.check_id,
        r.verdict.label(),
        r.verdict.explain()
    );
    match &r.controls {
        Some(c) => out.push_str(&format!(
            "  controls   violating {} p={:.2} {}   clean {} p={:.2} {}\n",
            c.violating.file,
            c.violating.p,
            if c.violating.fired {
                "fired ok"
            } else {
                "DID NOT FIRE"
            },
            c.clean.file,
            c.clean.p,
            if c.clean.fired {
                "FIRED — false positive"
            } else {
                "quiet ok"
            },
        )),
        None => out.push_str("  controls   none declared\n"),
    }
    out.push_str(&format!(
        "  history    {} of {} samples, fired {}, p median {:.2} min {:.2} max {:.2}\n",
        r.samples.len(),
        r.samples_wanted,
        r.fired,
        r.median_p,
        r.min_p,
        r.max_p,
    ));
    match &r.introduced_at {
        Some(commit) => out.push_str(&format!(
            "  sampled    only commits older than {}, where `{}` was introduced\n",
            &commit[..commit.len().min(8)],
            r.ask,
        )),
        None => out.push_str(
            "  sampled    the commit that introduced this question could not be found, \
             so these samples may include changes the question itself already approved\n",
        ),
    }
    if r.samples.len() < r.samples_wanted {
        out.push_str(&format!(
            "  short      wanted {} samples and found {}. History before the question ran out; \
             nothing newer was substituted.\n",
            r.samples_wanted,
            r.samples.len(),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(p: f64, verdict: &str) -> Sample {
        Sample {
            file: "a.rs".into(),
            commit: "abc".into(),
            p,
            verdict: verdict.into(),
        }
    }

    fn controls(violating_fired: bool, clean_fired: bool) -> Controls {
        Controls {
            violating: ControlResult {
                file: "violating.rs".into(),
                p: if violating_fired { 0.99 } else { 0.03 },
                verdict: if violating_fired { "FAIL" } else { "ok" }.into(),
                fired: violating_fired,
            },
            clean: ControlResult {
                file: "clean.rs".into(),
                p: if clean_fired { 0.91 } else { 0.06 },
                verdict: if clean_fired { "FAIL" } else { "ok" }.into(),
                fired: clean_fired,
            },
        }
    }

    /// The measurement this whole module exists for. These six numbers
    /// are the real answers `self_comparing` gave on six historic file
    /// states of this repo.
    fn measured_clean_history() -> Vec<Sample> {
        [0.19, 0.04, 0.07, 0.04, 0.08, 0.19]
            .into_iter()
            .map(|p| sample(p, if p > 0.5 { "FAIL" } else { "ok" }))
            .collect()
    }

    #[test]
    fn a_question_that_never_fires_on_clean_history_is_mute_not_approved() {
        let verdict = classify(
            &measured_clean_history(),
            Some(&controls(true, false)),
            Expect::No,
        );
        assert_eq!(verdict, Verdict::Mute);
        assert!(!verdict.needs_action());
    }

    /// The control doing the work it exists for: identical history, but
    /// the planted violation did not fire, so the question is broken and
    /// not merely quiet. Without controls these two cases are one case.
    #[test]
    fn same_history_with_a_failing_control_is_broken_not_mute() {
        let verdict = classify(
            &measured_clean_history(),
            Some(&controls(false, false)),
            Expect::No,
        );
        assert_eq!(verdict, Verdict::Broken);
    }

    #[test]
    fn a_control_that_fires_on_the_near_miss_is_broken() {
        let verdict = classify(
            &measured_clean_history(),
            Some(&controls(true, true)),
            Expect::No,
        );
        assert_eq!(verdict, Verdict::Broken);
    }

    #[test]
    fn no_controls_is_its_own_verdict_and_never_a_pass() {
        let verdict = classify(&measured_clean_history(), None, Expect::No);
        assert_eq!(verdict, Verdict::Uncontrolled);
        assert!(verdict.needs_action());
    }

    #[test]
    fn firing_on_most_of_history_is_noisy() {
        let samples: Vec<Sample> = (0..10)
            .map(|i| sample(0.9, if i < 7 { "FAIL" } else { "ok" }))
            .collect();
        assert_eq!(
            classify(&samples, Some(&controls(true, false)), Expect::No),
            Verdict::Noisy
        );
    }

    #[test]
    fn firing_on_a_minority_with_spread_is_decisive() {
        let mut samples = measured_clean_history();
        samples.push(sample(0.97, "FAIL"));
        assert_eq!(
            classify(&samples, Some(&controls(true, false)), Expect::No),
            Verdict::Decisive
        );
    }

    /// Not firing is not the same as not discriminating. A question that
    /// got close on one file separated it from the rest, and calling
    /// that mute would hide the one sample worth looking at.
    #[test]
    fn coming_close_without_firing_is_decisive_not_mute() {
        let mut samples = measured_clean_history();
        samples.push(sample(0.82, "UNSURE"));
        assert_eq!(
            classify(&samples, Some(&controls(true, false)), Expect::No),
            Verdict::Decisive
        );
    }

    #[test]
    fn too_few_samples_is_thin_rather_than_a_verdict_about_history() {
        let samples = vec![sample(0.04, "ok"), sample(0.06, "ok")];
        assert_eq!(
            classify(&samples, Some(&controls(true, false)), Expect::No),
            Verdict::Thin
        );
    }

    /// `expect: yes` inverts what counts as a violation. Reading `p`
    /// directly would call this history mute when every file in it is a
    /// violation.
    #[test]
    fn expect_yes_inverts_the_violation_probability() {
        let samples: Vec<Sample> = (0..6).map(|_| sample(0.02, "FAIL")).collect();
        assert_eq!(
            classify(&samples, Some(&controls(true, false)), Expect::Yes),
            Verdict::Noisy
        );
    }

    #[test]
    fn a_short_sample_is_reported_rather_than_topped_up() {
        let report = summarise(Run {
            check_id: "self-comparing".into(),
            ask: "self_comparing".into(),
            expect: Expect::No,
            introduced_at: Some("325574e035c0823e5a282765be58b6d88d028b6d".into()),
            samples_wanted: 20,
            samples: measured_clean_history(),
            controls: Some(controls(true, false)),
            model: None,
        });
        let text = render_text(&CalibrationRecord::new(0, "head".into(), vec![report]));
        assert!(text.contains("wanted 20 samples and found 6"));
        assert!(text.contains("nothing newer was substituted"));
        assert!(text.contains("only commits older than 325574e0"));
    }

    #[test]
    fn a_missing_boundary_is_said_out_loud() {
        let report = summarise(Run {
            check_id: "c".into(),
            ask: "q".into(),
            expect: Expect::No,
            introduced_at: None,
            samples_wanted: 20,
            samples: measured_clean_history(),
            controls: Some(controls(true, false)),
            model: None,
        });
        let text = render_text(&CalibrationRecord::new(0, "h".into(), vec![report]));
        assert!(text.contains("could not be found"));
        assert!(text.contains("already approved"));
    }

    #[test]
    fn mute_is_never_offered_as_an_edit() {
        let report = summarise(Run {
            check_id: "c".into(),
            ask: "q".into(),
            expect: Expect::No,
            introduced_at: Some("deadbeefcafe".into()),
            samples_wanted: 6,
            samples: measured_clean_history(),
            controls: Some(controls(true, false)),
            model: None,
        });
        assert!(report.suggested_edit.is_none());
        let text = render_text(&CalibrationRecord::new(0, "h".into(), vec![report]));
        assert!(text.contains("nothing to find here yet"));
        assert!(text.contains("This report edited no manifest"));
    }

    #[test]
    fn median_of_an_even_sample_is_the_mean_of_the_middle_two() {
        assert!((median(&[0.0, 0.1, 0.3, 0.5]) - 0.2).abs() < 1e-9);
        assert!((median(&[0.4]) - 0.4).abs() < 1e-9);
        assert!((median(&[]) - 0.0).abs() < 1e-9);
    }
}
