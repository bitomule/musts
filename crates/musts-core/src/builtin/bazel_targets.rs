//! What `musts run` needs to know about the targets of a `bazel/test`
//! command: whether an empty result is Bazel saying the patterns hold no
//! test, and how many of the tests that passed came from Bazel's cache.

/// Bazel's exit code for "the build passed but the patterns hold no test".
const NO_TESTS_FOUND_EXIT: i32 = 4;

/// Whether a `bazel test` that exited with `code` found no test to run
/// and every target it was given was a wildcard, so the empty result is
/// a vacuous pass. A named label that is not a test stays a failure.
pub fn no_tests_is_vacuous(argv: &[String], code: Option<i32>) -> bool {
    if code != Some(NO_TESTS_FOUND_EXIT) {
        return false;
    }
    let mut targets = argv
        .iter()
        .skip(2)
        .take_while(|a| !a.starts_with("--"))
        .map(|a| a.trim_start_matches('-'))
        .peekable();
    targets.peek().is_some() && targets.all(is_wildcard_pattern)
}

fn is_wildcard_pattern(raw: &str) -> bool {
    let Some(rest) = main_repo_pattern(raw.trim()) else {
        return false;
    };
    let (package, name) = match rest.split_once(':') {
        Some((package, name)) => (package, Some(name)),
        None => (rest, None),
    };
    let is_wild_name = |n: &str| matches!(n, "all" | "*" | "all-targets");
    if package == "..." || package.ends_with("/...") {
        return name.is_none_or(is_wild_name);
    }
    !package.contains("...") && name.is_some_and(is_wild_name)
}

fn main_repo_pattern(raw: &str) -> Option<&str> {
    let rest = raw
        .strip_prefix("@@")
        .or_else(|| raw.strip_prefix('@'))
        .unwrap_or(raw);
    rest.strip_prefix("//")
}

/// How many tests a `bazel test` run passed, and how many of those Bazel
/// served from its test cache instead of running.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PassedTests {
    pub total: usize,
    pub cached: usize,
}

/// Reads Bazel's per-target summary lines (`//a:t    (cached) PASSED in
/// 0.1s`, `//a:t    PASSED in 1.2s`) out of the console output.
pub fn passed_tests(output: &str) -> PassedTests {
    let mut passed = PassedTests::default();
    for line in output.lines() {
        let line = line.trim();
        if !(line.starts_with("//") || line.starts_with('@')) {
            continue;
        }
        let Some((_, verdict)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let verdict = verdict.trim_start();
        if verdict.starts_with("(cached) PASSED") {
            passed.total += 1;
            passed.cached += 1;
        } else if verdict.starts_with("PASSED") {
            passed.total += 1;
        }
    }
    passed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        ["bazel", "test"]
            .iter()
            .chain(args)
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn exit_4_over_wildcards_only_is_vacuous() {
        let a = argv(&["//a/...", "-//a/x/...", "//b:all", "--test_output=errors"]);
        assert!(no_tests_is_vacuous(&a, Some(4)));
    }

    #[test]
    fn exit_4_over_a_named_label_is_not_vacuous() {
        let a = argv(&["//a/...", "//b:lib", "--test_output=errors"]);
        assert!(!no_tests_is_vacuous(&a, Some(4)));
    }

    #[test]
    fn other_exit_codes_are_not_vacuous() {
        let a = argv(&["//a/...", "--test_output=errors"]);
        for code in [Some(0), Some(1), Some(3), None] {
            assert!(!no_tests_is_vacuous(&a, code), "{code:?}");
        }
    }

    #[test]
    fn counts_cached_and_executed_passes() {
        let output = "\
INFO: Build completed successfully, 3 total actions
//a:one                                                         (cached) PASSED in 0.3s
//a:two                                                                  PASSED in 1.2s
//b:three                                                       (cached) PASSED in 0.1s

Executed 1 out of 3 tests: 3 tests pass.
";
        assert_eq!(
            passed_tests(output),
            PassedTests {
                total: 3,
                cached: 2
            }
        );
    }

    #[test]
    fn failures_and_prose_are_not_counted() {
        let output = "//a:one   FAILED in 0.3s\nINFO: //a:two PASSED\n";
        assert_eq!(passed_tests(output), PassedTests::default());
    }
}
