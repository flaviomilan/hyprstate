//! Pairs expected windows (from a snapshot) with candidate windows (live, or
//! from another snapshot). Pure and deterministic.
//!
//! Scoring, per compatible pair:
//! - same [`AppIdentity`]                      → base 100 (strategy `identity`)
//! - else same initial class, neither a web app → base 50  (strategy `class`)
//! - exact title +20, same initial title +5, else title word overlap 0‥10
//! - same class +5, same workspace +3
//!
//! Pairs are assigned greedily by descending score; each candidate is used at
//! most once. PID is never used: it cannot survive a restart.

use std::collections::HashSet;
use std::fmt;
use std::path::Path;

use serde::Serialize;

use crate::discovery::resolve::{AppIdentity, Confidence};
use crate::hyprland::models::WorkspaceRef;

/// What the matcher needs to know about a window.
pub trait Matchable {
    fn identity(&self) -> &AppIdentity;
    fn confidence(&self) -> Confidence;
    fn class(&self) -> &str;
    fn initial_class(&self) -> &str;
    fn title(&self) -> &str;
    fn initial_title(&self) -> &str;
    fn workspace(&self) -> &WorkspaceRef;
    fn floating(&self) -> bool;
    /// Working directory of a live window (a terminal's shell), if known.
    fn cwd(&self) -> Option<&Path> {
        None
    }
    /// Hard conditions on candidates (expected side only).
    fn required_cwd(&self) -> Option<&Path> {
        None
    }
    fn required_title(&self) -> Option<&str> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    /// Same resolved application (desktop entry, flatpak, web app, executable).
    Identity,
    /// Same initial class only.
    Class,
}

impl fmt::Display for Strategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Identity => "application identity",
            Self::Class => "class",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Match {
    /// Index into `expected`.
    pub expected: usize,
    /// Index into `candidates`.
    pub candidate: usize,
    pub strategy: Strategy,
    pub confidence: Confidence,
    /// Title evidence contributed to the choice.
    pub title_match: bool,
    pub score: u32,
    /// Another candidate scored the same but would lead to a different result.
    pub ambiguous: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Assignment {
    pub matches: Vec<Match>,
    pub unmatched_expected: Vec<usize>,
    pub unmatched_candidates: Vec<usize>,
}

impl Assignment {
    pub fn for_expected(&self, i: usize) -> Option<&Match> {
        self.matches.iter().find(|m| m.expected == i)
    }
}

struct Scored {
    score: u32,
    strategy: Strategy,
    title_match: bool,
}

fn score<E: Matchable, C: Matchable>(e: &E, c: &C) -> Option<Scored> {
    if e.required_cwd().is_some_and(|want| c.cwd() != Some(want))
        || e.required_title().is_some_and(|want| !c.title().contains(want))
    {
        return None;
    }
    // Same app but a different initial class is a different kind of window
    // (e.g. `foot --app-id scratch` vs plain `foot`). Empty = not yet known.
    let class_compatible = e.initial_class().is_empty()
        || c.initial_class().is_empty()
        || e.initial_class().eq_ignore_ascii_case(c.initial_class());
    let strategy = if e.identity() == c.identity() && class_compatible {
        Strategy::Identity
    } else if !e.initial_class().is_empty()
        && e.initial_class().eq_ignore_ascii_case(c.initial_class())
        && !matches!(e.identity(), AppIdentity::WebApp { .. })
        && !matches!(c.identity(), AppIdentity::WebApp { .. })
    {
        Strategy::Class
    } else {
        return None;
    };
    let mut s = match strategy {
        Strategy::Identity => 100,
        Strategy::Class => 50,
    };
    let exact_title = !e.title().is_empty() && e.title() == c.title();
    let same_initial_title = !e.initial_title().is_empty() && e.initial_title() == c.initial_title();
    if exact_title {
        s += 20;
    }
    if same_initial_title {
        s += 5;
    }
    let overlap = if exact_title { 0 } else { word_overlap(e.title(), c.title()) };
    s += overlap;
    if e.class().eq_ignore_ascii_case(c.class()) {
        s += 5;
    }
    if e.workspace().same_as(c.workspace()) {
        s += 3;
    }
    Some(Scored { score: s, strategy, title_match: exact_title || overlap >= 5 })
}

/// 0‥10 by Jaccard similarity of lowercase words (length ≥ 2).
fn word_overlap(a: &str, b: &str) -> u32 {
    let words = |s: &str| -> HashSet<String> {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.chars().count() >= 2)
            .map(str::to_lowercase)
            .collect()
    };
    let (wa, wb) = (words(a), words(b));
    if wa.is_empty() || wb.is_empty() {
        return 0;
    }
    let inter = wa.intersection(&wb).count() as f64;
    let union = wa.union(&wb).count() as f64;
    (inter / union * 10.0).round() as u32
}

pub fn assign<E: Matchable, C: Matchable>(expected: &[E], candidates: &[C]) -> Assignment {
    let mut pairs: Vec<(usize, usize, Scored)> = Vec::new();
    for (ei, e) in expected.iter().enumerate() {
        for (ci, c) in candidates.iter().enumerate() {
            if let Some(s) = score(e, c) {
                pairs.push((ei, ci, s));
            }
        }
    }
    // Highest score first; ties broken by input order for determinism.
    pairs.sort_by(|a, b| b.2.score.cmp(&a.2.score).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));

    let mut used_e = vec![false; expected.len()];
    let mut used_c = vec![false; candidates.len()];
    let mut matches = Vec::new();
    for (ei, ci, s) in &pairs {
        if used_e[*ei] || used_c[*ci] {
            continue;
        }
        used_e[*ei] = true;
        used_c[*ci] = true;
        let e = &expected[*ei];
        let c = &candidates[*ci];
        let confidence = match s.strategy {
            Strategy::Identity => e.confidence().min(c.confidence()),
            Strategy::Class if s.title_match => Confidence::Medium,
            Strategy::Class => Confidence::Low,
        };
        matches.push(Match {
            expected: *ei,
            candidate: *ci,
            strategy: s.strategy,
            confidence,
            title_match: s.title_match,
            score: s.score,
            ambiguous: false,
        });
    }

    // Ambiguity: an equally scored rival candidate went to another expected
    // window with a different target, so swapping them would change the
    // result. Swapping identical twins (same target) is harmless.
    let owner: Vec<Option<usize>> = (0..candidates.len())
        .map(|ci| matches.iter().find(|m| m.candidate == ci).map(|m| m.expected))
        .collect();
    let same_target = |a: usize, b: usize| {
        expected[a].workspace().same_as(expected[b].workspace()) && expected[a].floating() == expected[b].floating()
    };
    for m in &mut matches {
        m.ambiguous = pairs.iter().any(|(ei, ci, s)| {
            *ei == m.expected
                && *ci != m.candidate
                && s.score == m.score
                && owner[*ci].is_some_and(|o| !same_target(o, m.expected))
        });
        if m.ambiguous {
            m.confidence = m.confidence.min(Confidence::Medium);
        }
    }
    matches.sort_by_key(|m| m.expected);

    Assignment {
        unmatched_expected: (0..expected.len()).filter(|i| !used_e[*i]).collect(),
        unmatched_candidates: (0..candidates.len()).filter(|i| !used_c[*i]).collect(),
        matches,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[derive(Debug, Clone)]
    pub struct W {
        pub id: AppIdentity,
        pub class: String,
        pub title: String,
        pub ws: WorkspaceRef,
        pub floating: bool,
    }

    impl Matchable for W {
        fn identity(&self) -> &AppIdentity {
            &self.id
        }
        fn confidence(&self) -> Confidence {
            Confidence::High
        }
        fn class(&self) -> &str {
            &self.class
        }
        fn initial_class(&self) -> &str {
            &self.class
        }
        fn title(&self) -> &str {
            &self.title
        }
        fn initial_title(&self) -> &str {
            ""
        }
        fn workspace(&self) -> &WorkspaceRef {
            &self.ws
        }
        fn floating(&self) -> bool {
            self.floating
        }
    }

    pub fn w(app: &str, title: &str, ws: i64) -> W {
        W {
            id: AppIdentity::Desktop { id: app.into() },
            class: app.into(),
            title: title.into(),
            ws: WorkspaceRef { id: ws, name: ws.to_string() },
            floating: false,
        }
    }

    #[test]
    fn counts_missing_windows() {
        // Snapshot: Firefox ×3; live: Firefox ×2 → one unmatched (to launch).
        let exp = [w("firefox", "a", 1), w("firefox", "b", 2), w("firefox", "c", 3)];
        let cur = [w("firefox", "b", 5), w("firefox", "zzz", 5)];
        let a = assign(&exp, &cur);
        assert_eq!(a.matches.len(), 2);
        assert_eq!(a.unmatched_expected.len(), 1);
        // Title evidence pairs "b" with "b".
        assert_eq!(a.for_expected(1).unwrap().candidate, 0);
        assert!(a.for_expected(1).unwrap().title_match);
    }

    #[test]
    fn different_apps_never_match() {
        let a = assign(&[w("code", "x", 1)], &[w("firefox", "x", 1)]);
        assert!(a.matches.is_empty());
        assert_eq!(a.unmatched_candidates, vec![0]);
    }

    #[test]
    fn identical_twins_are_not_ambiguous() {
        // Discord ×2 on the same workspace: any pairing is equivalent.
        let exp = [w("discord", "#general", 4), w("discord", "#random", 4)];
        let cur = [w("discord", "Discord", 9), w("discord", "Discord", 8)];
        let a = assign(&exp, &cur);
        assert_eq!(a.matches.len(), 2);
        assert!(a.matches.iter().all(|m| !m.ambiguous));
    }

    #[test]
    fn indistinguishable_windows_with_different_targets_are_ambiguous() {
        let exp = [w("discord", "#general", 4), w("discord", "#random", 5)];
        let cur = [w("discord", "Discord", 1), w("discord", "Discord", 2)];
        let a = assign(&exp, &cur);
        assert!(a.matches.iter().all(|m| m.ambiguous));
    }

    #[test]
    fn class_fallback_is_lower_confidence() {
        let mut e = w("foo", "t", 1);
        e.id = AppIdentity::Executable { path: "/usr/bin/foo".into() };
        let mut c = w("foo", "t", 1);
        c.id = AppIdentity::Class { class: "foo".into() };
        let a = assign(&[e], &[c]);
        assert_eq!(a.matches[0].strategy, Strategy::Class);
        assert_eq!(a.matches[0].confidence, Confidence::Medium);
    }

    #[test]
    fn same_app_with_custom_class_is_a_different_window() {
        let mut probe = w("foot", "x", 1);
        probe.class = "scratchpad".into();
        let a = assign(&[probe], &[w("foot", "x", 1)]);
        assert!(a.matches.is_empty());
    }

    struct Req(W, Option<&'static str>, Option<&'static str>);

    impl Matchable for Req {
        fn identity(&self) -> &AppIdentity { self.0.identity() }
        fn confidence(&self) -> Confidence { self.0.confidence() }
        fn class(&self) -> &str { self.0.class() }
        fn initial_class(&self) -> &str { self.0.initial_class() }
        fn title(&self) -> &str { self.0.title() }
        fn initial_title(&self) -> &str { self.0.initial_title() }
        fn workspace(&self) -> &WorkspaceRef { self.0.workspace() }
        fn floating(&self) -> bool { self.0.floating() }
        fn cwd(&self) -> Option<&Path> { self.1.map(Path::new) }
        fn required_cwd(&self) -> Option<&Path> { self.1.map(Path::new) }
        fn required_title(&self) -> Option<&str> { self.2 }
    }

    #[test]
    fn requirements_are_hard_constraints() {
        let want = Req(w("kitty", "", 2), Some("/p/recsys"), None);
        let other = Req(w("kitty", "x", 2), Some("/p/other"), None);
        let right = Req(w("kitty", "x", 7), Some("/p/recsys"), None);
        assert!(assign(std::slice::from_ref(&want), &[other]).matches.is_empty());
        assert_eq!(assign(&[want], &[right]).matches.len(), 1);

        let want = Req(w("firefox", "", 3), None, Some("Linear"));
        assert!(assign(std::slice::from_ref(&want), &[w("firefox", "YouTube", 3)]).matches.is_empty());
        assert_eq!(assign(&[want], &[w("firefox", "Linear - Issues", 3)]).matches.len(), 1);
    }

    #[test]
    fn exact_title_beats_workspace() {
        let exp = [w("kitty", "~/proj", 1)];
        let cur = [w("kitty", "~", 1), w("kitty", "~/proj", 7)];
        assert_eq!(assign(&exp, &cur).matches[0].candidate, 1);
    }
}
