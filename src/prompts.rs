// prompts.kdl (spec §19): every text the player writes into an agent's
// prompt file or pastes into its session, as named templates with `{name}`
// placeholders. Director-owned, required on main; `dashboard init` commits
// `STARTER` when main has none. Parsing validates the whole file: every
// template present, no unknown ones, only each template's own placeholders,
// every delivered prompt's static text within `max-bytes`, and every
// template's within its own or its section's `max-bytes=`.

use std::collections::BTreeMap;

/// The file's path on main.
pub const FILE: &str = "prompts.kdl";

/// The starter file `dashboard init` commits.
pub const STARTER: &str = include_str!("prompts.kdl");

/// Every template, as `section.name`, with the placeholders it may use.
const TEMPLATES: &[(&str, &[&str])] = &[
    ("worker.launch", &["slug", "project"]),
    ("worker.queued", &[]),
    ("worker.workspace", &["slug"]),
    ("worker.flaky", &["slug", "fix_paths", "replacements"]),
    ("worker.test-speedup", &["slow"]),
    ("worker.finish-accepted", &[]),
    ("worker.rejected", &["message", "recovery"]),
    ("worker.recovery-history", &["slug"]),
    ("worker.recovery-other", &[]),
    ("worker.review", &["round", "slug", "review"]),
    ("worker.contest-recorded", &["n"]),
    ("worker.contest-reply", &["n", "slug", "reply"]),
    ("worker.merge-failed", &["slug", "summary"]),
    ("worker.held", &["checks"]),
    ("worker.released", &["slug"]),
    ("spike.launch", &["slug", "project", "pinned"]),
    ("spike.question", &["round", "question"]),
    ("spike.finish-accepted", &["answer"]),
    ("spike.next-round", &["round", "slug", "findings", "claims"]),
    (
        "task-agent.preamble",
        &[
            "slug",
            "project",
            "phase",
            "task_toml",
            "brief",
            "report_file",
        ],
    ),
    ("task-agent.relaunched", &["slug"]),
    ("task-agent.queued", &[]),
    ("task-agent.post", &["slug", "check"]),
    ("task-agent.post-check-task", &["slug"]),
    ("task-agent.post-check-flaky", &[]),
    ("task-agent.post-check-speedup", &[]),
    (
        "task-agent.review",
        &["round", "slug", "tip", "base", "cap", "notes"],
    ),
    ("task-agent.review-notes", &["notes"]),
    ("task-agent.review-cap-last", &[]),
    ("task-agent.review-cap-over", &["max"]),
    ("task-agent.contest", &["n", "slug", "reason", "remaining"]),
    ("task-agent.wait", &["slug", "status"]),
    ("task-agent.rejected", &["message", "recovery"]),
    ("task-agent.recovery-history", &["slug"]),
    ("task-agent.recovery-other", &["slug"]),
    ("task-agent.rebase", &["slug"]),
    ("task-agent.hold", &["slug", "status"]),
    ("task-agent.imported-post", &["commit"]),
    ("task-agent.imported-send-back", &["commit"]),
    ("task-agent.imported-reply", &["commit"]),
    ("task-agent.imported-trim", &["commit"]),
    ("task-agent.escalated", &["slug", "cause"]),
    ("task-agent.question", &["question", "report_file"]),
];

/// What the player joins templates with when one prompt is made of several
/// (`Prompts::worker_launch` and friends); counted as static text too.
const JOIN: &str = "\n\n";
const RULE: &str = "\n\n---\n\n";

#[derive(Debug, Clone)]
pub struct Prompts {
    pub max_bytes: usize,
    texts: BTreeMap<&'static str, String>,
}

impl Prompts {
    /// Parses and validates a prompts.kdl.
    pub fn parse(text: &str) -> Result<Self, String> {
        let doc: kdl::KdlDocument = text.parse().map_err(|e: kdl::KdlError| {
            let details: Vec<String> = e
                .diagnostics
                .iter()
                .map(|d| {
                    format!(
                        "{} at byte {}",
                        d.message.clone().unwrap_or_else(|| "syntax error".into()),
                        d.span.offset()
                    )
                })
                .collect();
            format!(
                "{FILE}: {}",
                if details.is_empty() {
                    e.to_string()
                } else {
                    details.join("; ")
                }
            )
        })?;

        let mut max_bytes = None;
        let mut texts = BTreeMap::new();
        for node in doc.nodes() {
            let name = node.name().value();
            if name == "max-bytes" {
                let n = match node.entries() {
                    [e] if e.name().is_none() => e.value().as_integer().filter(|n| *n > 0),
                    _ => None,
                };
                let n = n.ok_or_else(|| format!("{FILE}: max-bytes takes one positive integer"))?;
                if max_bytes.replace(n as usize).is_some() {
                    return Err(format!("{FILE}: max-bytes set twice"));
                }
                continue;
            }
            let children = node.children().ok_or_else(|| {
                format!("{FILE}: unknown node {name:?}: expected max-bytes or a section ({{ … }})")
            })?;
            let (args, section_max) = entries(node.entries(), name)?;
            if !args.is_empty() {
                return Err(format!(
                    "{FILE}: section {name} takes only a max-bytes= property"
                ));
            }
            for child in children.nodes() {
                let key = format!("{name}.{}", child.name().value());
                let (static_key, vars) = TEMPLATES
                    .iter()
                    .find(|(k, _)| *k == key)
                    .ok_or_else(|| format!("{FILE}: unknown template {key}"))?;
                let (args, part_max) = entries(child.entries(), &key)?;
                let text = match args[..] {
                    [v] if child.children().is_none() => v.as_string(),
                    _ => None,
                };
                let text = text.ok_or_else(|| {
                    format!("{FILE}: {key} takes one string (and an optional max-bytes= property)")
                })?;
                if let Some(max) = part_max.or(section_max) {
                    let n = static_len(text);
                    if n > max {
                        return Err(format!(
                            "{FILE}: {key} is {n} bytes, over its max-bytes {max}"
                        ));
                    }
                }
                for p in placeholders(text) {
                    if !vars.contains(&p) {
                        let allowed = if vars.is_empty() {
                            "none".to_string()
                        } else {
                            vars.iter()
                                .map(|v| format!("{{{v}}}"))
                                .collect::<Vec<_>>()
                                .join(" ")
                        };
                        return Err(format!(
                            "{FILE}: {key}: unknown placeholder {{{p}}} (allowed: {allowed})"
                        ));
                    }
                }
                if texts.insert(*static_key, text.to_string()).is_some() {
                    return Err(format!("{FILE}: {key} defined twice"));
                }
            }
        }
        let missing: Vec<&str> = TEMPLATES
            .iter()
            .map(|(k, _)| *k)
            .filter(|k| !texts.contains_key(k))
            .collect();
        if !missing.is_empty() {
            return Err(format!(
                "{FILE}: missing template(s): {}",
                missing.join(", ")
            ));
        }
        let max_bytes = max_bytes.ok_or_else(|| format!("{FILE}: max-bytes is required"))?;
        let prompts = Prompts { max_bytes, texts };
        let over: Vec<String> = prompts
            .static_sizes()
            .into_iter()
            .filter(|(_, n)| *n > max_bytes)
            .map(|(what, n)| format!("{what} ({n} bytes)"))
            .collect();
        if !over.is_empty() {
            return Err(format!(
                "{FILE}: over max-bytes {max_bytes}: {}",
                over.join(", ")
            ));
        }
        Ok(prompts)
    }

    /// The template `key`, placeholders filled from `vars`.
    pub fn render(&self, key: &str, vars: &[(&str, &str)]) -> String {
        let text = self
            .texts
            .get(key)
            .unwrap_or_else(|| panic!("no template {key}"));
        let mut out = String::with_capacity(text.len());
        let mut rest = text.as_str();
        while let Some(start) = rest.find('{') {
            out.push_str(&rest[..start]);
            let after = &rest[start + 1..];
            match placeholder_at(after) {
                Some(name) => {
                    match vars.iter().find(|(k, _)| *k == name) {
                        Some((_, v)) => out.push_str(v),
                        None => debug_assert!(false, "{key}: {{{name}}} not given"),
                    }
                    rest = &after[name.len() + 1..];
                }
                None => {
                    out.push('{');
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// A worker's prompt file: its preamble (plus an ordinary task's sparse
    /// tree, a flaky-test-fix's or a test-speedup's paragraph, already
    /// rendered), then the task's prompt.md.
    pub fn worker_launch(&self, preamble: &str, kind: Option<&str>, prompt: &str) -> String {
        match kind {
            Some(f) => format!("{preamble}{JOIN}{f}{RULE}{prompt}"),
            None => format!("{preamble}{RULE}{prompt}"),
        }
    }

    /// A spike worker's prompt file: preamble, prompt.md, the question.
    pub fn spike_launch(&self, preamble: &str, prompt: &str, question: &str) -> String {
        format!("{preamble}{RULE}{prompt}{JOIN}{question}")
    }

    /// A task agent's prompt file: preamble (plus the relaunch note), then
    /// the step.
    pub fn agent_launch(&self, preamble: &str, relaunched: Option<&str>, step: &str) -> String {
        match relaunched {
            Some(r) => format!("{preamble}{JOIN}{r}{RULE}{step}"),
            None => format!("{preamble}{RULE}{step}"),
        }
    }

    /// Prepends `header` to a message's appended block (a spike's question).
    pub fn with_block(header: &str, block: &str) -> String {
        format!("{header}{JOIN}{block}")
    }

    /// The static size of every prompt the player can deliver, at its
    /// largest: each template alone, and each composition of several, with
    /// the joins between them. What fills placeholders isn't counted.
    pub fn static_sizes(&self) -> Vec<(String, usize)> {
        let s = |k: &str| static_len(&self.texts[k]);
        let max = |ks: &[&str]| ks.iter().map(|k| s(k)).max().unwrap_or(0);
        let mut sizes: Vec<(String, usize)> = TEMPLATES
            .iter()
            .map(|(k, _)| (k.to_string(), s(k)))
            .collect();
        let step = [
            s("task-agent.post")
                + max(&[
                    "task-agent.post-check-task",
                    "task-agent.post-check-flaky",
                    "task-agent.post-check-speedup",
                ]),
            s("task-agent.review")
                + max(&["task-agent.review-cap-last", "task-agent.review-cap-over"])
                + s("task-agent.review-notes"),
            s("task-agent.contest"),
            s("task-agent.wait"),
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        sizes.extend([
            (
                "worker prompt".to_string(),
                s("worker.launch")
                    + JOIN.len()
                    + s("worker.queued")
                    + JOIN.len()
                    + max(&["worker.workspace", "worker.flaky", "worker.test-speedup"])
                    + RULE.len(),
            ),
            (
                "spike prompt".to_string(),
                s("spike.launch") + RULE.len() + JOIN.len() + s("spike.question"),
            ),
            (
                "spike next round".to_string(),
                s("spike.next-round") + JOIN.len() + s("spike.question"),
            ),
            (
                "task agent prompt".to_string(),
                s("task-agent.preamble")
                    + JOIN.len()
                    + s("task-agent.relaunched")
                    + JOIN.len()
                    + s("task-agent.queued")
                    + RULE.len()
                    + step,
            ),
            ("task agent step".to_string(), step),
            (
                "worker rejection".to_string(),
                s("worker.rejected") + max(&["worker.recovery-history", "worker.recovery-other"]),
            ),
            (
                "task agent rejection".to_string(),
                s("task-agent.rejected")
                    + max(&["task-agent.recovery-history", "task-agent.recovery-other"]),
            ),
        ]);
        sizes
    }
}

/// A node's positional values and its optional `max-bytes=` property; any
/// other property is an error.
fn entries<'a>(
    entries: &'a [kdl::KdlEntry],
    what: &str,
) -> Result<(Vec<&'a kdl::KdlValue>, Option<usize>), String> {
    let mut args = Vec::new();
    let mut max = None;
    for e in entries {
        match e.name().map(|n| n.value()) {
            None => args.push(e.value()),
            Some("max-bytes") => {
                let n = e.value().as_integer().filter(|n| *n > 0).ok_or_else(|| {
                    format!("{FILE}: {what}: max-bytes= takes a positive integer")
                })?;
                if max.replace(n as usize).is_some() {
                    return Err(format!("{FILE}: {what}: max-bytes= set twice"));
                }
            }
            Some(other) => {
                return Err(format!(
                    "{FILE}: {what}: unknown property {other}= (only max-bytes=)"
                ));
            }
        }
    }
    Ok((args, max))
}

/// `text`'s length without its placeholders.
fn static_len(text: &str) -> usize {
    text.len()
        - placeholders(text)
            .iter()
            .map(|p| p.len() + 2)
            .sum::<usize>()
}

/// The `{name}` placeholders in `text` (`name` lowercase ASCII and `_`).
fn placeholders(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        match placeholder_at(after) {
            Some(name) => {
                out.push(name);
                rest = &after[name.len() + 1..];
            }
            None => rest = after,
        }
    }
    out
}

/// The placeholder name at the start of `s` (just past a `{`), if `s`
/// continues `name}`.
fn placeholder_at(s: &str) -> Option<&str> {
    let end = s.find(|c: char| !(c.is_ascii_lowercase() || c == '_'))?;
    (end > 0 && s[end..].starts_with('}')).then(|| &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_starter_validates() {
        let p = Prompts::parse(STARTER).unwrap();
        assert!(
            p.render(
                "worker.launch",
                &[("slug", "s"), ("project", "p")]
            )
            .starts_with("You are the worker for task `s` in project `p`.")
        );
    }

    #[test]
    fn render_fills_placeholders_and_keeps_other_braces() {
        let p = Prompts::parse(STARTER).unwrap();
        let t = p.render(
            "spike.launch",
            &[("slug", "probe"), ("project", "proj"), ("pinned", "  - x")],
        );
        assert!(t.contains("METRICS {\"throughput\": 2.1}"), "{t}");
        assert!(t.contains("tasks/probe/claims/"));
        assert!(!t.contains("{slug}"));
    }

    #[test]
    fn multiline_strings_are_dedented() {
        let p = Prompts::parse(STARTER).unwrap();
        let q = p.render(
            "spike.question",
            &[("round", "2"), ("question", "question = \"q\"")],
        );
        assert_eq!(
            q,
            "## The question (round 2)\n\n```toml\nquestion = \"q\"\n```"
        );
    }

    fn replace(from: &str, to: &str) -> String {
        assert!(STARTER.contains(from), "{from}");
        STARTER.replacen(from, to, 1)
    }

    #[test]
    fn an_unknown_placeholder_is_an_error() {
        let err = Prompts::parse(&replace(
            "{round} from the director",
            "{rnd} from the director",
        ))
        .unwrap_err();
        assert!(
            err.contains("worker.review: unknown placeholder {rnd}"),
            "{err}"
        );
    }

    #[test]
    fn missing_unknown_and_duplicate_templates_are_errors() {
        let err =
            Prompts::parse(&replace("    contest-recorded ", "    contest-recordd ")).unwrap_err();
        assert!(
            err.contains("unknown template worker.contest-recordd"),
            "{err}"
        );
        let err = Prompts::parse(&replace(
            "    recovery-other \"fix it with a new commit on top\"\n",
            "",
        ))
        .unwrap_err();
        assert!(
            err.contains("missing template(s): worker.recovery-other"),
            "{err}"
        );
        let err = Prompts::parse(&replace(
            "    recovery-other \"fix it with a new commit on top\"\n",
            "    recovery-other \"a\"\n    recovery-other \"b\"\n",
        ))
        .unwrap_err();
        assert!(err.contains("defined twice"), "{err}");
    }

    #[test]
    fn max_bytes_is_required_and_enforced() {
        let err = Prompts::parse(&replace("max-bytes 6144\n", "")).unwrap_err();
        assert!(err.contains("max-bytes is required"), "{err}");
        let err = Prompts::parse(&replace("max-bytes 6144", "max-bytes 500")).unwrap_err();
        assert!(
            err.contains("over max-bytes 500") && err.contains("task agent prompt"),
            "{err}"
        );
    }

    #[test]
    fn part_limits_come_from_the_template_or_its_section() {
        // A template's own max-bytes= wins over its section's.
        let err = Prompts::parse(&replace(
            "    finish-accepted \"finish",
            "    finish-accepted max-bytes=10 \"finish",
        ))
        .unwrap_err();
        assert!(
            err.contains("worker.finish-accepted is 80 bytes, over its max-bytes 10"),
            "{err}"
        );
        // The section's applies to the rest: launch keeps its own 1536.
        let err = Prompts::parse(&replace(
            "\nworker max-bytes=512 {",
            "\nworker max-bytes=100 {",
        ))
        .unwrap_err();
        assert!(
            err.contains("worker.review is 223 bytes, over its max-bytes 100"),
            "{err}"
        );
        let err = Prompts::parse(&replace(
            "    launch max-bytes=1536 #\"\"\"\n        You are the worker for task",
            "    launch max-bytes=1000 #\"\"\"\n        You are the worker for task",
        ))
        .unwrap_err();
        assert!(
            err.contains("worker.launch is 1531 bytes, over its max-bytes 1000"),
            "{err}"
        );
        let err = Prompts::parse(&replace(
            "\nworker max-bytes=512 {",
            "\nworker max-chars=1 {",
        ))
        .unwrap_err();
        assert!(err.contains("unknown property max-chars="), "{err}");
    }

    #[test]
    fn placeholder_content_is_not_counted() {
        let p = Prompts::parse(STARTER).unwrap();
        let big = "x".repeat(100_000);
        let sizes = p.static_sizes();
        let review = sizes.iter().find(|(k, _)| k == "worker.review").unwrap().1;
        let rendered = p.render(
            "worker.review",
            &[("round", ""), ("slug", ""), ("review", &big)],
        );
        assert_eq!(rendered.len() - big.len(), review);
    }

    #[test]
    fn a_syntax_error_names_its_position() {
        let err = Prompts::parse("max-bytes 10\nworker {\n").unwrap_err();
        assert!(err.starts_with("prompts.kdl: "), "{err}");
    }
}
