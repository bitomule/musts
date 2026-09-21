//! Emission-site extraction: the few lines a question is actually about.
//!
//! `uses: jev` sends one file per call and asks its question about the whole of it. For a
//! question about a *call's arguments* that is the wrong unit twice over: in a 2,000-line
//! feature file the one call that matters is 0.1% of the state, and the verdict comes back
//! per file, so a red cannot say which line. This module cuts the state down to the call and
//! the lines the call's arguments come from.
//!
//! It is deliberately a lexer and not a parser. A Swift parser would be correct about
//! constructs this never sees, and would also be a dependency, a build cost and a second
//! thing to keep current with the language. What is here is measured against the shape the
//! five apps actually use — 731 emission sites, one form — and anything it cannot resolve it
//! drops rather than guesses, because a site reported at the wrong line is worse than a site
//! not reported at all.

/// What kind of place the model is being asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteKind {
    /// A call that emits an event: `tracking.execute(.boxCreated(...))`.
    Call,
    /// A `case` of the app's own event enum, where a parameter is declared.
    EventCase,
}

impl SiteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SiteKind::Call => "call",
            SiteKind::EventCase => "event-case",
        }
    }
}

/// One place worth asking about, with the lines its values come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Site {
    /// 1-indexed line of the first character of `text`.
    pub line: usize,
    pub kind: SiteKind,
    /// The call or case declaration, parentheses balanced, newlines preserved.
    pub text: String,
    /// The lines above it. Provenance lives here: whether `name` was typed by the user or
    /// read off an enum is almost never visible in the call itself.
    pub context: String,
}

/// Method names that emit. The receiver is what decides, not these — `useCase.execute(...)`
/// is most of an app and none of it is analytics.
const EMIT_METHODS: [&str; 4] = ["execute", "track", "send", "signal"];

/// A receiver whose name carries one of these is an analytics client. Measured across the
/// five apps: `tracking`, `trackingClient`, `trackingUseCase`, `analytics`, `telemetry`.
const RECEIVER_MARKERS: [&str; 3] = ["track", "analytics", "telemetry"];

/// How many lines above a site travel with it. Six covers the `let` that names the value and
/// the `guard` that unwraps it; more starts dragging in the rest of the function, which is the
/// dilution this module exists to undo.
const CONTEXT_LINES: usize = 6;

/// Every emission site in one Swift source.
///
/// `path` decides whether event-case declarations are in scope: the enum that declares them
/// is one file per app and it is named for what it is.
pub fn swift_analytics(path: &str, source: &str) -> Vec<Site> {
    let mut sites = calls(source);
    if declares_events(path, source) {
        sites.extend(event_cases(source));
    }
    sites.sort_by_key(|s| s.line);
    sites
}

fn declares_events(path: &str, source: &str) -> bool {
    path.to_lowercase().contains("trackingevent") || source.contains(": TrackingEvent")
}

/// Byte offset -> 1-indexed line.
fn line_of(source: &str, offset: usize) -> usize {
    source[..offset].bytes().filter(|b| *b == b'\n').count() + 1
}

fn context_above(source: &str, line: usize) -> String {
    let lines: Vec<&str> = source.lines().collect();
    let end = line.saturating_sub(1);
    let start = end.saturating_sub(CONTEXT_LINES);
    lines[start..end].join("\n")
}

/// True when the offset sits inside a `//` line comment.
fn in_line_comment(source: &str, offset: usize) -> bool {
    let start = source[..offset].rfind('\n').map_or(0, |i| i + 1);
    source[start..offset].contains("//")
}

fn calls(source: &str) -> Vec<Site> {
    let mut out = Vec::new();
    let bytes = source.as_bytes();
    for method in EMIT_METHODS {
        let needle = format!(".{method}(");
        let mut from = 0;
        while let Some(rel) = source[from..].find(&needle) {
            let dot = from + rel;
            from = dot + needle.len();
            if in_line_comment(source, dot) {
                continue;
            }
            let Some(receiver_start) = receiver_start(bytes, dot) else {
                continue;
            };
            let receiver = &source[receiver_start..dot];
            let lowered = receiver.to_lowercase();
            if !RECEIVER_MARKERS.iter().any(|m| lowered.contains(m)) {
                continue;
            }
            let open = dot + needle.len() - 1;
            let Some(close) = balanced_end(source, open) else {
                continue;
            };
            let line = line_of(source, receiver_start);
            out.push(Site {
                line,
                kind: SiteKind::Call,
                text: source[receiver_start..=close].to_string(),
                context: context_above(source, line),
            });
        }
    }
    out
}

/// Walk back over the identifier chain in front of the dot. `None` when there is nothing to
/// walk back over — `.execute(` after `{` is a trailing-closure shorthand, not a receiver.
fn receiver_start(bytes: &[u8], dot: usize) -> Option<usize> {
    let mut i = dot;
    while i > 0 {
        let c = bytes[i - 1];
        if c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || c == b'?' || c == b'!' {
            i -= 1;
        } else {
            break;
        }
    }
    (i < dot).then_some(i)
}

/// The offset of the `)` that closes the `(` at `open`, skipping string literals so a `)`
/// inside one cannot unbalance the scan. `None` when the file is truncated or the shape is
/// one this lexer cannot follow — the site is then dropped, not guessed at.
fn balanced_end(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_string => i += 1,
            b'"' => in_string = !in_string,
            b'(' if !in_string => depth += 1,
            b')' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn event_cases(source: &str) -> Vec<Site> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for raw in source.lines() {
        let line_start = offset;
        offset += raw.len() + 1;
        let trimmed = raw.trim_start();
        if !trimmed.starts_with("case ") {
            continue;
        }
        // A `switch` arm is not a declaration, and the enum's own `eventName` and `parameters`
        // functions are made of them. Measured on Boxy's event enum: without this the file
        // reports 69 sites where it declares 31, and two thirds of what the guard would pay to
        // judge is the same parameter list read a second time. An arm matches on a case, so it
        // is spelled `case .boxCreated` or `case let .boxCreated`; a declaration names one.
        let after = trimmed["case ".len()..].trim_start();
        if after.starts_with('.') || after.starts_with("let ") || after.starts_with("var ") {
            continue;
        }
        // Only cases that carry values. A bare `case appLaunched` sends nothing and asking
        // about it is noise with no finding available to it.
        let Some(paren) = raw.find('(') else {
            continue;
        };
        let open = line_start + paren;
        let Some(close) = balanced_end(source, open) else {
            continue;
        };
        // The second half of the same discriminator: an arm ends in `:`, a declaration does
        // not. `case .a, .b:` has no dot on the second alternative and would slip past the
        // check above.
        if source[close + 1..]
            .lines()
            .next()
            .is_some_and(|rest| rest.trim_start().starts_with(':'))
        {
            continue;
        }
        let start = line_start + (raw.len() - trimmed.len());
        let line = line_of(source, start);
        out.push(Site {
            line,
            kind: SiteKind::EventCase,
            text: source[start..=close].to_string(),
            context: context_above(source, line),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_call_on_an_analytics_receiver() {
        let src = "let n = box.category.name\ntracking.execute(.boxCreated(categoryName: n))\n";
        let sites = swift_analytics("Boxy/UI/BoxFeature.swift", src);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].kind, SiteKind::Call);
        assert_eq!(sites[0].line, 2);
        assert_eq!(
            sites[0].text,
            "tracking.execute(.boxCreated(categoryName: n))"
        );
        assert!(sites[0].context.contains("box.category.name"));
    }

    #[test]
    fn ignores_a_call_on_any_other_receiver() {
        let src = "loadBoxesUseCase.execute(.all)\n";
        assert!(swift_analytics("Boxy/UI/BoxFeature.swift", src).is_empty());
    }

    #[test]
    fn ignores_a_call_inside_a_comment() {
        let src = "// tracking.execute(.boxCreated(name: n))\n";
        assert!(swift_analytics("Boxy/UI/BoxFeature.swift", src).is_empty());
    }

    #[test]
    fn follows_a_call_across_lines_and_over_a_paren_in_a_string() {
        let src = "trackingUseCase.execute(\n    .purchaseFailed(\n        reason: \"declined (card)\"\n    )\n)\n";
        let sites = swift_analytics("FoodLabel/UI/Paywall.swift", src);
        assert_eq!(sites.len(), 1);
        assert!(sites[0].text.ends_with(")\n)"));
        assert!(sites[0].text.contains("declined (card)"));
    }

    #[test]
    fn reads_event_cases_only_in_the_event_enum() {
        let src = "public enum BoxyTrackingEvent: TrackingEvent {\n    case appLaunched\n    case boxCreated(categoryName: String)\n}\n";
        let sites = swift_analytics("Boxy/Domain/Models/BoxyTrackingEvents.swift", src);
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].kind, SiteKind::EventCase);
        assert_eq!(sites[0].text, "case boxCreated(categoryName: String)");

        // The same text in a file that declares no events is not an emission site.
        assert!(swift_analytics("Boxy/UI/Other.swift", "    case boxCreated(x: Int)\n").is_empty());
    }

    #[test]
    fn a_switch_arm_is_not_a_declaration() {
        let src = "public enum E: TrackingEvent {\n    case boxCreated(name: String)\n    var parameters: [String: String] {\n        switch self {\n        case .boxCreated(let name):\n            return [:]\n        case let .other(a):\n            return [:]\n        }\n    }\n}\n";
        let sites = swift_analytics("E.swift", src);
        assert_eq!(sites.len(), 1, "{sites:#?}");
        assert_eq!(sites[0].text, "case boxCreated(name: String)");
    }

    #[test]
    fn a_file_with_nothing_to_ask_about_yields_nothing() {
        let src = "struct Box { let name: String }\n";
        assert!(swift_analytics("Boxy/Domain/Box.swift", src).is_empty());
    }
}
