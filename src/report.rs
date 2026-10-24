// The foreman's report (spec §17). The player notes in the foreman's
// worktree git dir which step waits for a report (`Owed`): the step it
// pasted (post, review, contest), the commit it imported from the foreman
// (posted, sent-back, replied) or the director's question.
// `dashboard report` checks the foreman's verdict against it and writes the
// report there atomically; the player collects it, fills in slug, step and
// commit from its own note, and turns it into a note and an event.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The step waiting for a report, written by the player.
const STEP_FILE: &str = "dashboard-step.json";
/// The director's question waiting for an answer, written by `dashboard ask`.
const QUESTION_FILE: &str = "dashboard-question.json";
/// The report, written by `dashboard report`, consumed by the player.
const REPORT_FILE: &str = "dashboard-report.json";

/// A foreman's verdict on its step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Posted,
    Accept,
    SentBack,
    Replied,
    Unsure,
    Answer,
}

impl Verdict {
    pub const ALL: [Verdict; 6] = [
        Verdict::Posted,
        Verdict::Accept,
        Verdict::SentBack,
        Verdict::Replied,
        Verdict::Unsure,
        Verdict::Answer,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Posted => "posted",
            Verdict::Accept => "accept",
            Verdict::SentBack => "sent-back",
            Verdict::Replied => "replied",
            Verdict::Unsure => "unsure",
            Verdict::Answer => "answer",
        }
    }

    pub fn parse(s: &str) -> Option<Verdict> {
        Verdict::ALL.into_iter().find(|v| v.as_str() == s)
    }
}

/// A step waiting for the foreman's report: what the player asked or
/// imported, and the verdicts that close it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owed {
    /// `post`, `review r<N>`, `contest c<N>` or `question`.
    pub step: String,
    /// The foreman's commit the player imported for this step.
    pub commit: Option<String>,
    pub verdicts: Vec<Verdict>,
}

impl Owed {
    /// A pasted step: post, review or contest. `unsure` closes any of them;
    /// a review also closes with `accept`. Its commit, once imported,
    /// replaces it (`Owed::imported`).
    pub fn asked(step: String) -> Owed {
        let verdicts = if step.starts_with("review") {
            vec![Verdict::Accept, Verdict::Unsure]
        } else {
            vec![Verdict::Unsure]
        };
        Owed {
            step,
            commit: None,
            verdicts,
        }
    }

    /// The foreman's imported commit: its verdict follows from the import.
    pub fn imported(step: String, commit: &str, verdict: Verdict) -> Owed {
        Owed {
            step,
            commit: Some(commit.to_string()),
            verdicts: vec![verdict],
        }
    }

    fn question() -> Owed {
        Owed {
            step: "question".to_string(),
            commit: None,
            verdicts: vec![Verdict::Answer, Verdict::Unsure],
        }
    }

    fn allowed(&self) -> String {
        let v: Vec<&str> = self.verdicts.iter().map(|v| v.as_str()).collect();
        format!("{} ({})", self.step, v.join(" | "))
    }
}

/// What the foreman hands `dashboard report`.
#[derive(Debug, Clone, Default)]
pub struct Draft {
    /// Left out after an import: the import's verdict.
    pub verdict: Option<String>,
    pub why: String,
    pub check: Option<String>,
    pub suite: Option<String>,
    pub concerns: Option<String>,
}

/// A report as `dashboard report` writes it: checked against `step`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filed {
    pub step: String,
    pub verdict: Verdict,
    pub why: String,
    pub check: Option<String>,
    pub suite: Option<String>,
    pub concerns: Option<String>,
}

fn read_owed(git_dir: &Path, file: &str) -> Option<Owed> {
    let text = std::fs::read_to_string(git_dir.join(file)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

fn write_owed(git_dir: &Path, file: &str, owed: &Owed) -> Result<(), String> {
    let json = serde_json::to_string(owed).map_err(|e| e.to_string())?;
    write_atomic(&git_dir.join(file), &json)
}

/// Notes `owed` as the step waiting for a report, in place of the last one.
pub fn set_step(git_dir: &Path, owed: &Owed) -> Result<(), String> {
    write_owed(git_dir, STEP_FILE, owed)
}

/// Notes the director's question as waiting for an answer.
pub fn set_question(git_dir: &Path) -> Result<(), String> {
    write_owed(git_dir, QUESTION_FILE, &Owed::question())
}

/// What waits for a report: the question first, then the step.
fn pending(git_dir: &Path) -> Vec<Owed> {
    [QUESTION_FILE, STEP_FILE]
        .iter()
        .filter_map(|f| read_owed(git_dir, f))
        .collect()
}

/// Checks `draft` against what waits in `git_dir`: a named verdict closes
/// the first pending step that allows it; a left-out one is the verdict of
/// the step's imported commit.
fn check(pending: &[Owed], draft: &Draft) -> Result<Filed, String> {
    if pending.is_empty() {
        return Err(
            "no step waits for a report: the player pastes your next step into this session"
                .to_string(),
        );
    }
    let list = pending
        .iter()
        .map(Owed::allowed)
        .collect::<Vec<_>>()
        .join(", ");
    let (owed, verdict) = match draft.verdict.as_deref().map(str::trim) {
        Some(name) => {
            let verdict = Verdict::parse(&name.to_ascii_lowercase()).ok_or_else(|| {
                format!("{name:?} is no verdict; waiting for a report: {list}")
            })?;
            let owed = pending
                .iter()
                .find(|o| o.verdicts.contains(&verdict))
                .ok_or_else(|| {
                    let hint = if matches!(
                        verdict,
                        Verdict::Posted | Verdict::SentBack | Verdict::Replied
                    ) {
                        "; that verdict follows from your commit: report it once the player pastes that it imported it"
                    } else {
                        ""
                    };
                    format!("{} closes no pending step; waiting for a report: {list}{hint}", verdict.as_str())
                })?;
            (owed, verdict)
        }
        None => {
            let owed = pending.iter().find(|o| o.commit.is_some()).ok_or_else(|| {
                format!("name your verdict; waiting for a report: {list}")
            })?;
            (owed, owed.verdicts[0])
        }
    };
    let why = draft.why.trim();
    if why.is_empty() {
        return Err("--why is required: two or three sentences".to_string());
    }
    let suite = draft.suite.as_deref().map(str::trim).filter(|s| !s.is_empty());
    match (verdict, suite) {
        (Verdict::Accept, None) => {
            return Err("accept needs --suite: each test this task added, keep or drop, and why".to_string());
        }
        (v, Some(_)) if v != Verdict::Accept => {
            return Err("--suite goes with accept only".to_string());
        }
        _ => {}
    }
    let opt = |s: &Option<String>| s.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    Ok(Filed {
        step: owed.step.clone(),
        verdict,
        why: why.to_string(),
        check: opt(&draft.check),
        suite: suite.map(String::from),
        concerns: opt(&draft.concerns),
    })
}

/// `dashboard report`: checks `draft` against what waits in `git_dir` (a
/// foreman's worktree git dir) and writes the report atomically for
/// the player to collect.
pub fn submit(git_dir: &Path, draft: &Draft) -> Result<Filed, String> {
    let filed = check(&pending(git_dir), draft)?;
    let json = serde_json::to_string(&filed).map_err(|e| e.to_string())?;
    write_atomic(&git_dir.join(REPORT_FILE), &json)?;
    Ok(filed)
}

/// A collected report: its verdict and the text the director reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collected {
    pub verdict: Verdict,
    pub text: String,
}

/// The player's side: takes the report waiting in `git_dir`, if any. It
/// closes the step it names (its note is removed) and comes back with slug,
/// step and commit filled in from the player's own note. A report the
/// pending steps don't allow (hand-written, or overtaken by a new step) is
/// removed and comes back as an error.
pub fn take(git_dir: &Path, slug: &str) -> Option<Result<Collected, String>> {
    let file = git_dir.join(REPORT_FILE);
    let text = std::fs::read_to_string(&file).ok()?;
    let _ = std::fs::remove_file(&file);
    let filed: Filed = match serde_json::from_str(&text) {
        Ok(f) => f,
        Err(e) => return Some(Err(format!("unreadable report: {e}"))),
    };
    let (name, owed) = match [QUESTION_FILE, STEP_FILE]
        .into_iter()
        .filter_map(|f| read_owed(git_dir, f).map(|o| (f, o)))
        .find(|(_, o)| o.step == filed.step && o.verdicts.contains(&filed.verdict))
    {
        Some(found) => found,
        None => {
            return Some(Err(format!(
                "your report ({} on {}) closes no pending step",
                filed.verdict.as_str(),
                filed.step
            )));
        }
    };
    let _ = std::fs::remove_file(git_dir.join(name));
    Some(Ok(Collected {
        verdict: filed.verdict,
        text: render(slug, &owed, &filed),
    }))
}

/// The block the director reads.
fn render(slug: &str, owed: &Owed, filed: &Filed) -> String {
    let or = |s: &Option<String>, d: &str| s.clone().unwrap_or_else(|| d.to_string());
    format!(
        "slug: {slug}\nstep: {}\ncommit: {}\ncheck: {}\nverdict: {}\nwhy: {}\nsuite: {}\nconcerns: {}",
        owed.step,
        owed.commit.as_deref().unwrap_or("none"),
        or(&filed.check, "n/a"),
        filed.verdict.as_str(),
        filed.why,
        or(&filed.suite, "n/a"),
        or(&filed.concerns, "none"),
    )
}

/// The `why` of a report's text as `render` writes it.
pub fn why_of(text: &str) -> Option<&str> {
    let start = text.find("\nwhy: ")? + "\nwhy: ".len();
    let rest = &text[start..];
    let why = rest.find("\nsuite: ").map_or(rest, |end| &rest[..end]).trim();
    (!why.is_empty()).then_some(why)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(verdict: Option<&str>) -> Draft {
        Draft {
            verdict: verdict.map(String::from),
            why: "because.".into(),
            ..Draft::default()
        }
    }

    #[test]
    fn a_review_takes_accept_or_unsure() {
        let review = vec![Owed::asked("review r1".into())];
        let mut d = draft(Some("accept"));
        assert!(check(&review, &d).unwrap_err().contains("--suite"));
        d.suite = Some("keep t1".into());
        assert_eq!(check(&review, &d).unwrap().verdict, Verdict::Accept);
        assert_eq!(check(&review, &draft(Some("Unsure"))).unwrap().verdict, Verdict::Unsure);
        let err = check(&review, &draft(Some("sent-back"))).unwrap_err();
        assert!(err.contains("review r1 (accept | unsure)") && err.contains("imported"), "{err}");
        assert!(check(&review, &draft(Some("maybe"))).unwrap_err().contains("no verdict"));
        assert!(check(&review, &draft(None)).unwrap_err().contains("name your verdict"));
    }

    #[test]
    fn an_import_implies_its_verdict() {
        let posted = vec![Owed::imported("post".into(), "abc", Verdict::Posted)];
        assert_eq!(check(&posted, &draft(None)).unwrap().verdict, Verdict::Posted);
        assert_eq!(check(&posted, &draft(Some("posted"))).unwrap().verdict, Verdict::Posted);
        assert!(check(&posted, &draft(Some("accept"))).is_err());
        assert!(check(&[], &draft(None)).unwrap_err().contains("no step waits"));
    }

    #[test]
    fn a_question_comes_first_and_the_step_stays() {
        let both = vec![Owed::question(), Owed::asked("review r2".into())];
        assert_eq!(check(&both, &draft(Some("answer"))).unwrap().step, "question");
        assert_eq!(check(&both, &draft(Some("unsure"))).unwrap().step, "question");
        let mut d = draft(Some("accept"));
        d.suite = Some("keep".into());
        assert_eq!(check(&both, &d).unwrap().step, "review r2");
        let mut d = draft(Some("answer"));
        d.why = "  ".into();
        assert!(check(&both, &d).unwrap_err().contains("--why"));
    }

    #[test]
    fn take_fills_in_slug_step_and_commit_and_closes_the_step() {
        let dir = tempfile::tempdir().unwrap();
        let g = dir.path();
        assert!(take(g, "s").is_none());
        set_step(g, &Owed::imported("review r1".into(), "abc123", Verdict::SentBack)).unwrap();
        let mut d = draft(None);
        d.concerns = Some("none here".into());
        submit(g, &d).unwrap();
        let got = take(g, "s").unwrap().unwrap();
        assert_eq!(got.verdict, Verdict::SentBack);
        assert!(got.text.contains("slug: s\nstep: review r1\ncommit: abc123"), "{}", got.text);
        assert!(got.text.contains("verdict: sent-back\nwhy: because.\nsuite: n/a\nconcerns: none here"));
        assert!(take(g, "s").is_none(), "consumed");
        assert!(submit(g, &draft(None)).is_err(), "the step is closed");
    }

    #[test]
    fn the_why_reads_back_from_the_rendered_text() {
        let filed = check(&[Owed::asked("review r1".into())], &Draft {
            verdict: Some("accept".into()),
            why: "it holds.\nTwo lines.".into(),
            suite: Some("keep t1".into()),
            ..Draft::default()
        })
        .unwrap();
        let text = render("s", &Owed::asked("review r1".into()), &filed);
        assert_eq!(why_of(&text), Some("it holds.\nTwo lines."));
        assert_eq!(why_of("slug: s"), None);
    }
}
