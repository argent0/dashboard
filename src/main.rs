// Console dashboard for a project's docs/roadmap/tasks.yaml.
//
// Shared across projects — lives outside any single repo. Run it from inside
// a project directory (or pass --file) and it renders that project's roadmap;
// the header title comes from tasks.yaml's own `title` field.
//
// Zero dependencies on purpose: many of the projects this runs against have no
// packaging set up for the language they're written in, so the parser below
// only understands the specific YAML subset tasks.yaml actually uses (flat
// mappings, flow lists, folded block scalars, and scalar block lists with
// folded continuation lines) rather than pulling in a real YAML library.
//
// Statuses: pending, in_progress (first pass), in_revision (sent back by a
// coordinator review for corrections), built, tested, blocked. Optional
// per-task `reviews:` list records each coordinator assessment as
// "r<N> <sent-back|accepted|escalated> <date> <notes>". The REVIEW_CAP-th
// sent-back round is flagged as the final one: if that revision still isn't
// right, the coordinator escalates to a human instead of sending it back again.

use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

// --------------------------------------------------------------------------
// Minimal YAML-subset parser
// --------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Yaml {
    Null,
    Bool(bool),
    Str(String),
    List(Vec<Yaml>),
    Mapping(HashMap<String, Yaml>),
}

impl Yaml {
    fn get(&self, key: &str) -> Option<&Yaml> {
        match self {
            Yaml::Mapping(m) => m.get(key),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Yaml::Str(s) => Some(s),
            _ => None,
        }
    }

    fn truthy(&self) -> bool {
        match self {
            Yaml::Null => false,
            Yaml::Bool(b) => *b,
            Yaml::Str(s) => !s.is_empty(),
            Yaml::List(v) => !v.is_empty(),
            Yaml::Mapping(m) => !m.is_empty(),
        }
    }

    fn as_str_list(&self) -> Vec<String> {
        match self {
            Yaml::List(v) => v.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(),
            _ => vec![],
        }
    }

    fn as_item_list(&self) -> &[Yaml] {
        match self {
            Yaml::List(v) => v.as_slice(),
            _ => &[],
        }
    }
}

fn get_str<'a>(m: &'a Yaml, key: &str, default: &'a str) -> &'a str {
    m.get(key).and_then(Yaml::as_str).unwrap_or(default)
}

fn get_list(m: &Yaml, key: &str) -> Vec<String> {
    m.get(key).map(Yaml::as_str_list).unwrap_or_default()
}

fn get_truthy(m: &Yaml, key: &str) -> bool {
    m.get(key).map(Yaml::truthy).unwrap_or(false)
}

fn strip_comment(line: &str) -> String {
    let mut in_quote: Option<char> = None;
    for (idx, ch) in line.char_indices() {
        if let Some(q) = in_quote {
            if ch == q {
                in_quote = None;
            }
        } else if ch == '"' || ch == '\'' {
            in_quote = Some(ch);
        } else if ch == '#' {
            return line[..idx].to_string();
        }
    }
    line.to_string()
}

fn indent_of(line: &str) -> usize {
    line.chars().take_while(|&c| c == ' ').count()
}

fn parse_scalar(s: &str) -> Yaml {
    let s = s.trim();
    if s.is_empty() {
        return Yaml::Null;
    }
    if s == "true" {
        return Yaml::Bool(true);
    }
    if s == "false" {
        return Yaml::Bool(false);
    }
    if s.starts_with('[') && s.ends_with(']') {
        let inner = s[1..s.len() - 1].trim();
        if inner.is_empty() {
            return Yaml::List(vec![]);
        }
        let items = inner
            .split(',')
            .map(|x| Yaml::Str(x.trim().trim_matches(|c| c == '"' || c == '\'').to_string()))
            .collect();
        return Yaml::List(items);
    }
    let chars: Vec<char> = s.chars().collect();
    if chars.len() >= 2 && chars[0] == chars[chars.len() - 1] && (chars[0] == '"' || chars[0] == '\'') {
        let inner: String = chars[1..chars.len() - 1].iter().collect();
        return Yaml::Str(inner);
    }
    Yaml::Str(s.to_string())
}

/// Matches `^(\w+):\s*(.*)$`, returning (key, value.strip()).
fn match_kv(content: &str) -> Option<(String, String)> {
    let key: String = content.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    if key.is_empty() {
        return None;
    }
    let rest = &content[key.len()..];
    let rest = rest.strip_prefix(':')?;
    Some((key, rest.trim().to_string()))
}

fn parse_scalar_list(lines: &[String], start: usize, dash_indent: usize) -> (Vec<String>, usize) {
    let n = lines.len();
    let mut items = Vec::new();
    let mut cur: Option<Vec<String>> = None;
    let mut i = start;
    while i < n {
        let l = &lines[i];
        if l.trim().is_empty() {
            i += 1;
            continue;
        }
        let ind = indent_of(l);
        let body = &l[ind..];
        if ind == dash_indent && body.starts_with("- ") {
            if let Some(c) = cur.take() {
                items.push(c.join(" ").trim().to_string());
            }
            cur = Some(vec![body[2..].trim().to_string()]);
            i += 1;
        } else if ind > dash_indent {
            cur.as_mut().expect("scalar list continuation before first item").push(l.trim().to_string());
            i += 1;
        } else {
            break;
        }
    }
    if let Some(c) = cur {
        items.push(c.join(" ").trim().to_string());
    }
    (items, i)
}

fn parse_field(item: &mut HashMap<String, Yaml>, content: &str, field_indent: usize, i: usize, lines: &[String]) -> usize {
    let n = lines.len();
    let (key, val) = match match_kv(content) {
        Some(kv) => kv,
        None => return i,
    };
    let mut i = i;
    if val == ">" || val == "|" {
        let mut block: Vec<String> = Vec::new();
        let mut block_indent: Option<usize> = None;
        while i < n {
            let l = &lines[i];
            if l.trim().is_empty() {
                i += 1;
                continue;
            }
            let ind = indent_of(l);
            if ind <= field_indent {
                break;
            }
            let bi = *block_indent.get_or_insert(ind);
            block.push(l[bi..].to_string());
            i += 1;
        }
        let joined = block.iter().map(|s| s.trim()).collect::<Vec<_>>().join(" ").trim().to_string();
        item.insert(key, Yaml::Str(joined));
    } else if val.is_empty() {
        let mut j = i;
        while j < n && lines[j].trim().is_empty() {
            j += 1;
        }
        if j < n && indent_of(&lines[j]) > field_indent && lines[j].trim_start().starts_with("- ") {
            let (values, new_i) = parse_scalar_list(lines, j, indent_of(&lines[j]));
            item.insert(key, Yaml::List(values.into_iter().map(Yaml::Str).collect()));
            i = new_i;
        } else {
            item.insert(key, Yaml::Null);
            i = j;
        }
    } else {
        item.insert(key, parse_scalar(&val));
    }
    i
}

fn parse_mapping_item(lines: &[String], i: usize, indent: usize) -> (HashMap<String, Yaml>, usize) {
    let n = lines.len();
    let mut item = HashMap::new();
    let body = &lines[i][indent..];
    let field_indent = indent + 2;
    let mut i = parse_field(&mut item, &body[2..], field_indent, i + 1, lines);
    while i < n {
        let l = &lines[i];
        if l.trim().is_empty() {
            i += 1;
            continue;
        }
        let ind = indent_of(l);
        if ind != field_indent {
            break;
        }
        let content = &l[field_indent..];
        if content.starts_with("- ") {
            break;
        }
        i = parse_field(&mut item, content, field_indent, i + 1, lines);
    }
    (item, i)
}

fn parse_list(lines: &[String], start: usize, indent: usize) -> (Vec<HashMap<String, Yaml>>, usize) {
    let n = lines.len();
    let mut items = Vec::new();
    let mut i = start;
    while i < n {
        let l = &lines[i];
        if l.trim().is_empty() {
            i += 1;
            continue;
        }
        if indent_of(l) != indent || !l[indent..].starts_with("- ") {
            break;
        }
        let (item, new_i) = parse_mapping_item(lines, i, indent);
        items.push(item);
        i = new_i;
    }
    (items, i)
}

fn load_yaml_subset(text: &str) -> HashMap<String, Yaml> {
    let lines: Vec<String> = text.lines().map(strip_comment).collect();
    let n = lines.len();
    let mut result = HashMap::new();
    let mut i = 0;
    while i < n {
        let l = lines[i].clone();
        if l.trim().is_empty() || indent_of(&l) != 0 {
            i += 1;
            continue;
        }
        let (key, val) = match l.split_once(':') {
            Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
            None => (l.trim().to_string(), String::new()),
        };
        i += 1;
        if !val.is_empty() {
            result.insert(key, parse_scalar(&val));
            continue;
        }
        let mut j = i;
        while j < n && lines[j].trim().is_empty() {
            j += 1;
        }
        if j < n && lines[j].trim_start().starts_with("- ") {
            let ind = indent_of(&lines[j]);
            let (items, new_i) = parse_list(&lines, j, ind);
            result.insert(key, Yaml::List(items.into_iter().map(Yaml::Mapping).collect()));
            i = new_i;
        } else {
            result.insert(key, Yaml::Null);
            i = j;
        }
    }
    result
}

// --------------------------------------------------------------------------
// Styling
// --------------------------------------------------------------------------

mod c {
    pub const RESET: &str = "\x1b[0m";
    pub const BOLD: &str = "\x1b[1m";
    pub const DIM: &str = "\x1b[2m";
    pub const GREEN: &str = "\x1b[32m";
    pub const CYAN: &str = "\x1b[36m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const RED: &str = "\x1b[31m";
    pub const GRAY: &str = "\x1b[90m";
    pub const MAGENTA: &str = "\x1b[35m";
    pub const WHITE: &str = "\x1b[97m";
}

struct StatusStyle {
    icon: &'static str,
    color: &'static str,
    label: &'static str,
}

fn status_style(status: &str) -> StatusStyle {
    match status {
        "tested" => StatusStyle { icon: "✓", color: c::GREEN, label: "tested" },
        "built" => StatusStyle { icon: "◆", color: c::CYAN, label: "built" },
        "in_progress" => StatusStyle { icon: "◐", color: c::YELLOW, label: "in_progress" },
        "in_revision" => StatusStyle { icon: "↻", color: c::MAGENTA, label: "in_revision" },
        "blocked" => StatusStyle { icon: "✗", color: c::RED, label: "blocked" },
        _ => StatusStyle { icon: "○", color: c::GRAY, label: "pending" },
    }
}

const STATUS_ORDER: [&str; 6] = ["tested", "built", "in_progress", "in_revision", "blocked", "pending"];

fn style(text: &str, codes: &[&str], enabled: bool) -> String {
    if !enabled {
        return text.to_string();
    }
    format!("{}{}{}", codes.join(""), text, c::RESET)
}

fn bar(fraction: f64, width: usize, enabled: bool) -> String {
    let filled = (fraction * width as f64).round();
    let filled = filled.max(0.0).min(width as f64) as usize;
    let s = "█".repeat(filled) + &"░".repeat(width - filled);
    if !enabled {
        return s;
    }
    let color = if fraction >= 1.0 {
        c::GREEN
    } else if fraction > 0.0 {
        c::CYAN
    } else {
        c::GRAY
    };
    format!("{color}{s}{}", c::RESET)
}

fn truncate(s: &str, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= width {
        return s.to_string();
    }
    if width <= 1 {
        return chars.into_iter().take(width).collect();
    }
    let mut out: String = chars.into_iter().take(width - 1).collect();
    out.push('…');
    out
}

fn center(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        return s.to_string();
    }
    let marg = width - len;
    let extra = if marg % 2 == 1 && width % 2 == 1 { 1 } else { 0 };
    let left = marg / 2 + extra;
    let right = marg - left;
    format!("{}{}{}", " ".repeat(left), s, " ".repeat(right))
}

// --------------------------------------------------------------------------
// Rendering
// --------------------------------------------------------------------------

// A `reviews:` entry is the coordinator's assessment of one pass over a task:
// "r<N> <sent-back|accepted|escalated> <date> <free text>". Sent-back rounds
// are capped: after the REVIEW_CAP-th, the next step is a human, not a round.
const REVIEW_CAP: usize = 2;

/// Matches `^r(\d+)\s+(sent-back|accepted|escalated)\b`, returning the verdict.
fn review_verdict(s: &str) -> Option<&'static str> {
    let rest = s.strip_prefix('r')?;
    let digit_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    if digit_end == 0 {
        return None;
    }
    let after_digits = &rest[digit_end..];
    let ws_len = after_digits.chars().take_while(|c| c.is_whitespace()).count();
    if ws_len == 0 {
        return None;
    }
    let after_ws = &after_digits[ws_len..];
    for verdict in ["sent-back", "accepted", "escalated"] {
        if let Some(after) = after_ws.strip_prefix(verdict) {
            let boundary_ok = match after.chars().next() {
                None => true,
                Some(ch) => !(ch.is_alphanumeric() || ch == '_'),
            };
            if boundary_ok {
                return Some(match verdict {
                    "sent-back" => "sent-back",
                    "accepted" => "accepted",
                    _ => "escalated",
                });
            }
        }
    }
    None
}

fn render(data: &Yaml, color: bool, width: usize) -> String {
    let phases = data.get("phases").map(Yaml::as_item_list).unwrap_or(&[]);
    let tasks = data.get("tasks").map(Yaml::as_item_list).unwrap_or(&[]);
    let by_id: HashMap<&str, &Yaml> = tasks.iter().filter_map(|t| t.get("id").and_then(Yaml::as_str).map(|id| (id, t))).collect();

    let mut out: Vec<String> = Vec::new();

    let task_status = |t: &Yaml| -> String { get_str(t, "status", "pending").to_string() };

    let reviews = |t: &Yaml| -> Vec<String> { get_list(t, "reviews") };

    let sent_back_rounds = |t: &Yaml| -> usize { reviews(t).iter().filter(|r| review_verdict(r) == Some("sent-back")).count() };

    let last_verdict = |t: &Yaml| -> Option<&'static str> { reviews(t).iter().rev().find_map(|r| review_verdict(r)) };

    let cap_hit = |t: &Yaml| -> bool { sent_back_rounds(t) >= REVIEW_CAP && last_verdict(t) == Some("sent-back") };

    let actionable = |t: &Yaml| -> bool {
        if task_status(t) != "pending" {
            return false;
        }
        get_list(t, "depends_on").iter().all(|d| by_id.get(d.as_str()).map(|dep| get_str(dep, "status", "pending") == "tested").unwrap_or(false))
    };

    let unmet_deps = |t: &Yaml| -> Vec<String> {
        get_list(t, "depends_on").into_iter().filter(|d| by_id.get(d.as_str()).map(|dep| get_str(dep, "status", "pending") != "tested").unwrap_or(true)).collect()
    };

    // ---- header ----
    let project = get_str(data, "title", "PROJECT");
    let title = format!(" {project} — Implementation Roadmap ");
    let inner_w = (width.saturating_sub(2)).max(title.chars().count());
    out.push(style(&format!("╭{}╮", "─".repeat(inner_w)), &[c::MAGENTA], color));
    out.push(format!(
        "{}{}{}",
        style("│", &[c::MAGENTA], color),
        style(&center(&title, inner_w), &[c::BOLD, c::WHITE], color),
        style("│", &[c::MAGENTA], color)
    ));
    out.push(style(&format!("╰{}╯", "─".repeat(inner_w)), &[c::MAGENTA], color));
    out.push(String::new());

    // ---- overall progress ----
    let total = tasks.len();
    let mut counts: HashMap<&str, usize> = STATUS_ORDER.iter().map(|s| (*s, 0)).collect();
    for t in tasks {
        let s = task_status(t);
        let key = STATUS_ORDER.iter().find(|&&o| o == s).copied().unwrap_or("pending");
        *counts.entry(key).or_insert(0) += 1;
    }
    let tested = *counts.get("tested").unwrap_or(&0);
    let frac = if total > 0 { tested as f64 / total as f64 } else { 0.0 };
    let pct = (frac * 100.0).round() as i64;

    let bar_w = 40.min(width.saturating_sub(30));
    let summary_bits = STATUS_ORDER
        .iter()
        .map(|s| {
            let ss = status_style(s);
            style(&format!("{} {} {}", ss.icon, counts.get(s).unwrap_or(&0), ss.label), &[ss.color], color)
        })
        .collect::<Vec<_>>()
        .join("  ");
    out.push(format!("{}  {}  {}/{} tested ({}%)", style("Overall", &[c::BOLD], color), bar(frac, bar_w, color), tested, total, pct));
    out.push(summary_bits);
    out.push(String::new());

    // ---- phases ----
    for ph in phases {
        let ph_id = get_str(ph, "id", "");
        let ph_tasks: Vec<&Yaml> = tasks.iter().filter(|t| get_str(t, "phase", "") == ph_id).collect();
        let ph_total = ph_tasks.len();
        let ph_tested = ph_tasks.iter().filter(|t| task_status(t) == "tested").count();
        let ph_frac = if ph_total > 0 { ph_tested as f64 / ph_total as f64 } else { 0.0 };
        let header = match ph.get("title") {
            Some(Yaml::Str(s)) => s.clone(),
            Some(Yaml::Null) | None => ph_id.to_string(),
            Some(_) => ph_id.to_string(),
        };
        out.push(format!("{}  {}  {}/{}", style(&header, &[c::BOLD], color), bar(ph_frac, 20, color), ph_tested, ph_total));
        let goal = ph.get("goal").and_then(Yaml::as_str).unwrap_or("");
        if !goal.is_empty() {
            out.push(format!("  {}", style(&truncate(goal, width.saturating_sub(2)), &[c::DIM], color)));
        }
        for t in &ph_tasks {
            let status = task_status(t);
            let ss = status_style(&status);
            let mark = if get_truthy(t, "milestone") { " ⚑" } else { "  " };
            let id = get_str(t, "id", "");
            let tid = format!("{:<24}", id);
            let tstatus = format!("{:<11}", status);
            let n_rev = reviews(t).len();
            let badge = if n_rev > 0 {
                format!("{} ", style(&format!("r{n_rev}"), &[if cap_hit(t) { c::RED } else { c::MAGENTA }], color))
            } else {
                String::new()
            };
            let badge_w = if n_rev > 0 { format!("r{n_rev} ").chars().count() } else { 0 };
            let title_w = (width.saturating_sub(46).saturating_sub(badge_w)).max(10);
            let row = format!(
                "  {} {} {}{} {}",
                ss.icon,
                style(&tstatus, &[ss.color], color),
                style(&tid, &[c::DIM], color),
                mark,
                badge + &truncate(get_str(t, "title", ""), title_w)
            );
            out.push(row);
        }
        out.push(String::new());
    }

    // ---- milestones ----
    let milestones: Vec<&Yaml> = tasks.iter().filter(|t| get_truthy(t, "milestone")).collect();
    if !milestones.is_empty() {
        out.push(style("⚑ Milestones", &[c::BOLD, c::MAGENTA], color));
        for t in &milestones {
            let status = task_status(t);
            let ss = status_style(&status);
            out.push(format!("  {} {} {}", ss.icon, style(&format!("{:<11}", status), &[ss.color], color), truncate(get_str(t, "title", ""), width.saturating_sub(20))));
        }
        out.push(String::new());
    }

    // ---- actionable now ("next", computed) ----
    let next_tasks: Vec<&Yaml> = tasks.iter().filter(|t| actionable(t)).collect();
    out.push(style(&format!("▶ Actionable now ({})", next_tasks.len()), &[c::BOLD, c::CYAN], color));
    if !next_tasks.is_empty() {
        for t in &next_tasks {
            let id = get_str(t, "id", "");
            out.push(format!("  {}  {}", style(id, &[c::CYAN], color), truncate(get_str(t, "title", ""), width.saturating_sub(id.chars().count()).saturating_sub(6))));
        }
    } else {
        out.push(style("  (none — everything actionable is in progress, or nothing is unblocked)", &[c::DIM], color));
    }
    out.push(String::new());

    // ---- in revision (sent back by the coordinator's review) ----
    let revising: Vec<&Yaml> = tasks.iter().filter(|t| task_status(t) == "in_revision").collect();
    out.push(style(&format!("↻ In revision ({})", revising.len()), &[c::BOLD, c::MAGENTA], color));
    if !revising.is_empty() {
        for t in &revising {
            let id = get_str(t, "id", "");
            let rv = reviews(t);
            let last = rv.last().cloned().unwrap_or_else(|| "no review recorded".to_string());
            let warn = if cap_hit(t) {
                style(&format!("  ⚠ final round ({REVIEW_CAP}/{REVIEW_CAP}) — escalate if still not right"), &[c::RED], color)
            } else {
                String::new()
            };
            let last_w = (width.saturating_sub(id.chars().count()).saturating_sub(8)).max(20);
            out.push(format!("  {}{}{}", style(id, &[c::MAGENTA], color), warn, style(&format!("  — {}", truncate(&last, last_w)), &[c::DIM], color)));
        }
    } else {
        out.push(style("  (none)", &[c::DIM], color));
    }
    out.push(String::new());

    // ---- blocked ----
    let blocked: Vec<&Yaml> = tasks.iter().filter(|t| task_status(t) == "blocked").collect();
    out.push(style(&format!("⛔ Blocked ({})", blocked.len()), &[c::BOLD, c::RED], color));
    if !blocked.is_empty() {
        for t in &blocked {
            let id = get_str(t, "id", "");
            let reason = match t.get("blocked_reason").and_then(Yaml::as_str) {
                Some(r) if !r.is_empty() => r.to_string(),
                _ => {
                    let deps = unmet_deps(t);
                    if deps.is_empty() { "no reason recorded".to_string() } else { format!("waiting on: {}", deps.join(", ")) }
                }
            };
            out.push(format!("  {}  {}{}", style(id, &[c::RED], color), truncate(get_str(t, "title", ""), 40), style(&format!("  — {reason}"), &[c::DIM], color)));
        }
    } else {
        out.push(style("  (none)", &[c::DIM], color));
    }

    out.join("\n")
}

// --------------------------------------------------------------------------
// Terminal size
// --------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod term {
    use std::os::unix::io::AsRawFd;

    #[repr(C)]
    struct Winsize {
        ws_row: u16,
        ws_col: u16,
        ws_xpixel: u16,
        ws_ypixel: u16,
    }

    const TIOCGWINSZ: u64 = 0x5413;

    unsafe extern "C" {
        fn ioctl(fd: i32, request: u64, argp: *mut Winsize) -> i32;
    }

    pub fn columns() -> Option<u32> {
        let mut ws = Winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
        let fd = std::io::stdout().as_raw_fd();
        let ret = unsafe { ioctl(fd, TIOCGWINSZ, &mut ws) };
        if ret == 0 && ws.ws_col > 0 {
            Some(ws.ws_col as u32)
        } else {
            None
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod term {
    pub fn columns() -> Option<u32> {
        None
    }
}

fn detect_width(explicit: Option<u32>) -> usize {
    let width = explicit
        .or_else(|| std::env::var("COLUMNS").ok().and_then(|v| v.parse::<u32>().ok()).filter(|&v| v > 0))
        .or_else(term::columns)
        .unwrap_or(96);
    width.clamp(60, 140) as usize
}

// --------------------------------------------------------------------------
// CLI
// --------------------------------------------------------------------------

fn default_tasks_file() -> Option<PathBuf> {
    for candidate in [Path::new("docs/roadmap/tasks.yaml"), Path::new("tasks.yaml")] {
        if candidate.exists() {
            return std::fs::canonicalize(candidate).ok();
        }
    }
    None
}

fn print_help() {
    println!(
        "usage: dashboard [-h] [--file FILE] [--no-color] [--width WIDTH]\n\n\
Console dashboard for a project's docs/roadmap/tasks.yaml.\n\n\
options:\n  \
-h, --help       show this help message and exit\n  \
--file FILE      Path to tasks.yaml (default: ./docs/roadmap/tasks.yaml or\n                   \
./tasks.yaml, resolved against the current directory).\n  \
--no-color       Disable ANSI colors.\n  \
--width WIDTH    Override detected terminal width."
    );
}

struct Args {
    file: Option<PathBuf>,
    no_color: bool,
    width: Option<u32>,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut file = None;
    let mut no_color = false;
    let mut width = None;
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        if arg == "-h" || arg == "--help" {
            print_help();
            std::process::exit(0);
        } else if arg == "--no-color" {
            no_color = true;
            i += 1;
        } else if let Some(v) = arg.strip_prefix("--file=") {
            file = Some(PathBuf::from(v));
            i += 1;
        } else if arg == "--file" {
            i += 1;
            let v = argv.get(i).ok_or("argument --file: expected one argument")?;
            file = Some(PathBuf::from(v));
            i += 1;
        } else if let Some(v) = arg.strip_prefix("--width=") {
            width = Some(v.parse::<u32>().map_err(|_| format!("argument --width: invalid int value: '{v}'"))?);
            i += 1;
        } else if arg == "--width" {
            i += 1;
            let v = argv.get(i).ok_or("argument --width: expected one argument")?;
            width = Some(v.parse::<u32>().map_err(|_| format!("argument --width: invalid int value: '{v}'"))?);
            i += 1;
        } else {
            return Err(format!("unrecognized arguments: {arg}"));
        }
    }
    Ok(Args { file, no_color, width })
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    let file = match args.file.or_else(default_tasks_file) {
        Some(f) => f,
        None => {
            eprintln!("error: no tasks.yaml found (tried ./docs/roadmap/tasks.yaml and ./tasks.yaml) — pass --file");
            std::process::exit(1);
        }
    };
    if !file.exists() {
        eprintln!("error: {} not found", file.display());
        std::process::exit(1);
    }

    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {} could not be read: {e}", file.display());
            std::process::exit(1);
        }
    };

    let data = Yaml::Mapping(load_yaml_subset(&text));
    let width = detect_width(args.width);
    let color = !args.no_color && std::io::stdout().is_terminal();

    println!("{}", render(&data, color, width));
}
