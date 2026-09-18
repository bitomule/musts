//! What the code resolves on jev's behalf.
//!
//! jev judges what is written in front of it and abstains when the answer depends on what
//! something resolves to at runtime: the same ten hollow tests scored 0/10 with ten
//! abstentions on raw source, and 10/10 once this module supplied the resolved count.
//!
//! Getting this right is the hard part, not the model. A wrong facts program does not
//! invent greens — jev abstains — it produces false REDS, which burn a check's credibility
//! in a week. The shell prototype of this module was wrong four times, and each error first
//! looked like a jev mistake. Every one of them is a test below.

use serde_json::{json, Value};

/// Blank out string literals BEFORE stripping comments.
///
/// A Rust string holding a Bazel label is not a comment, and treating it as one deletes the
/// rest of the line: `assert_eq!(target_slug("//App:Target"), …)` read as assertion-free and
/// a healthy three-assertion test was flagged at p=0.90.
fn strip(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            '"' => {
                out.push('"');
                i += 1;
                while i < b.len() && b[i] != '"' {
                    if b[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
                out.push('"');
            }
            '/' if i + 1 < b.len() && b[i + 1] == '/' => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < b.len() && b[i + 1] == '*' => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Byte indices throughout, deliberately.
///
/// The first version mixed a byte offset from `str::find` with an index into a `Vec<char>`
/// and then let the depth counter underflow, which panicked on the first file holding a
/// non-ASCII byte before a brace. Braces are ASCII, so scanning bytes is both correct and
/// simpler — and a facts program that panics takes the whole check down with it.
fn balanced_body(src: &str, from: usize) -> Option<(usize, usize)> {
    let b = src.as_bytes();
    let open = from + src.get(from..)?.find('{')?;
    let mut depth = 0usize;
    for (i, c) in b.iter().enumerate().skip(open) {
        match c {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some((open, i + 1));
                }
            }
            _ => {}
        }
    }
    None
}

fn count(hay: &str, needles: &[&str]) -> usize {
    needles.iter().map(|n| hay.matches(n).count()).sum()
}

/// Every way this language can fail a test. Enumerate them all before counting anything:
/// the shell version missed panics and fluent builders and called healthy tests hollow.
fn assertions(body: &str) -> (usize, usize, usize, usize) {
    let explicit = count(body, &["assert!", "assert_eq!", "assert_ne!"]);
    // In Rust a panic ends the test red, so these are assertions too.
    let panicking = count(body, &[".unwrap()", ".expect(", "panic!", "unreachable!"]);
    // Fluent builders (assert_cmd, predicates) never say `assert!`.
    let fluent = count(
        body,
        &[
            ".assert()",
            ".success()",
            ".failure()",
            ".stdout(",
            ".stderr(",
            ".code(",
        ],
    );
    let self_comparing = body
        .match_indices("assert_eq!(")
        .filter(|(i, _)| {
            let rest = &body[i + "assert_eq!(".len()..];
            let end = rest.find(')').unwrap_or(0);
            let args: Vec<&str> = rest[..end].split(',').map(str::trim).collect();
            args.len() >= 2 && args[0] == args[1]
        })
        .count();
    (explicit, panicking, fluent, self_comparing)
}

/// Follow the pointer jev cannot: a loop over a bare identifier is traced back to its `let`.
fn loops_enclosing_assertions(body: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for (i, _) in body.match_indices("for ") {
        let rest = &body[i + 4..];
        let Some(inpos) = rest.find(" in ") else {
            continue;
        };
        let Some(brace) = rest.find('{') else {
            continue;
        };
        if brace < inpos {
            continue;
        }
        let expr = rest[inpos + 4..brace].trim().to_string();
        let tail_end = (brace + 600).min(rest.len());
        if !rest[brace..tail_end].contains("assert") {
            continue;
        }
        let mut resolved = expr.clone();
        for _ in 0..4 {
            if !resolved.chars().all(|c| c.is_alphanumeric() || c == '_') || resolved.is_empty() {
                break;
            }
            let pat = format!("let {resolved}");
            let pat_mut = format!("let mut {resolved}");
            let Some(at) = body.find(&pat).or_else(|| body.find(&pat_mut)) else {
                break;
            };
            let Some(eq) = body[at..].find('=') else {
                break;
            };
            let line_end = body[at + eq..].find(['\n', ';']).unwrap_or(0);
            resolved = body[at + eq + 1..at + eq + line_end].trim().to_string();
        }
        let empty = ["vec![]", "Vec::new()", "[]", "&[]"]
            .iter()
            .any(|e| resolved.replace(' ', "").starts_with(e));
        let mut entry = json!({ "expression": expr, "resolved_element_count": Value::Null });
        if empty {
            entry["resolved_element_count"] = json!(0);
        }
        if resolved != expr {
            entry["expression_resolves_to"] = json!(resolved);
        }
        out.push(entry);
    }
    out
}

/// The deterministic filter and the resolved facts, for one Rust source file.
///
/// `applicable` is false when the file declares no `#[test]` at all. That is not a judgment
/// call, it is a question that does not apply: 81% of this check's original abstentions were
/// such files, one wasted request each.
pub fn rust_tests(src: &str) -> Value {
    let s = strip(src);
    let mut tests = Vec::new();
    let mut at = 0usize;
    while let Some(found) = s[at..].find("#[test]") {
        let start = at + found;
        let Some(fnpos) = s[start..].find("fn ") else {
            break;
        };
        let name_start = start + fnpos + 3;
        let name: String = s[name_start..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let Some((open, close)) = balanced_body(&s, name_start) else {
            break;
        };
        let body = &s[open..close];
        let (explicit, panicking, fluent, self_comparing) = assertions(body);
        tests.push(json!({
            "test_name": name,
            "assertion_count": explicit + panicking + fluent,
            "explicit_assertions": explicit,
            "panicking_calls": panicking,
            "fluent_assertions": fluent,
            "self_comparing_assertions": self_comparing,
            "loops_enclosing_assertions": loops_enclosing_assertions(body),
        }));
        at = close;
    }
    json!({ "applicable": !tests.is_empty(), "tests": tests })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test here is an error this module actually made. The shell prototype was wrong
    /// four times and this one twice more, and every single time it first looked like a jev
    /// mistake — three of them at p >= 0.90. A wrong facts program produces false REDS.
    fn one(src: &str) -> Value {
        rust_tests(src).get("tests").unwrap().as_array().unwrap()[0].clone()
    }

    #[test]
    fn a_string_holding_a_double_slash_is_not_a_comment() {
        // Stripping `//` comments first ate the rest of this line, so a healthy
        // three-assertion test read as assertion-free and was flagged at p=0.90.
        let t = one(r#"#[test] fn t() { assert_eq!(slug("//App:Target"), "app-target"); }"#);
        assert_eq!(t["assertion_count"], 1);
        assert_eq!(t["explicit_assertions"], 1);
    }

    #[test]
    fn unwrap_is_an_assertion_because_a_panic_ends_the_test_red() {
        let t = one("#[test] fn t() { validate(&p()).unwrap(); }");
        assert_eq!(t["assertion_count"], 1);
        assert_eq!(t["explicit_assertions"], 0);
        assert_eq!(t["panicking_calls"], 1);
    }

    #[test]
    fn fluent_builders_never_say_assert() {
        // assert_cmd is most of a CLI suite and the first version saw none of it.
        let t = one(r#"#[test] fn t() { bin().arg("x").assert().failure().code(2); }"#);
        assert!(t["fluent_assertions"].as_u64().unwrap() >= 2);
        assert!(t["assertion_count"].as_u64().unwrap() >= 2);
    }

    #[test]
    fn a_self_comparing_assertion_is_counted() {
        let t = one("#[test] fn t() { assert_eq!(total, total); }");
        assert_eq!(t["self_comparing_assertions"], 1);
    }

    #[test]
    fn a_loop_over_an_empty_vec_resolves_to_zero_through_its_let() {
        let t = one(
            "#[test] fn t() { let empty: Vec<u8> = Vec::new(); for _ in empty { assert!(x); } }",
        );
        let l = &t["loops_enclosing_assertions"][0];
        assert_eq!(l["resolved_element_count"], 0);
        assert_eq!(l["expression_resolves_to"], "Vec::new()");
    }

    #[test]
    fn a_loop_whose_length_only_runtime_knows_is_left_unresolved() {
        // The honest answer is null, not a guess: jev abstains on it, which is correct.
        let t = one("#[test] fn t() { for _ in listdir(p) { assert!(x); } }");
        assert!(t["loops_enclosing_assertions"][0]["resolved_element_count"].is_null());
    }

    #[test]
    fn a_file_with_no_test_is_not_applicable() {
        assert_eq!(
            rust_tests("pub fn helper() -> u8 { 1 }")["applicable"],
            false
        );
    }

    #[test]
    fn an_attribute_between_test_and_fn_does_not_hide_the_test() {
        // The Python prototype required `fn` on the very next line and silently dropped 14
        // real test files, which made the measured abstention rate wrong in both directions.
        let v = rust_tests("#[test]\n#[ignore]\nfn t() { assert!(x); }");
        assert_eq!(v["applicable"], true);
        assert_eq!(v["tests"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn a_stray_close_brace_does_not_panic() {
        // Mixing a byte offset with an index into a Vec<char> and letting the depth counter
        // underflow panicked on a real file, and a facts program that panics takes the whole
        // check down with it.
        let _ = rust_tests("} #[test] fn t() { assert!(x); }");
        let _ = rust_tests("// ñ á é\n#[test] fn t() { assert!(x); }");
    }
}
