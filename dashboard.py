#!/usr/bin/env python3
"""Console dashboard for a project's docs/roadmap/tasks.yaml.

Shared across projects — lives outside any single repo. Run it from inside
a project directory (or pass --file) and it renders that project's roadmap;
the header title comes from tasks.yaml's own `title` field.

Zero dependencies on purpose: many of the projects this runs against have no
Python packaging set up, so the parser below only understands the specific
YAML subset tasks.yaml actually uses (flat mappings, flow lists, folded
block scalars, and scalar block lists with folded continuation lines)
rather than pulling in PyYAML.

Statuses: pending, in_progress (first pass), in_revision (sent back by a
coordinator review for corrections), built, tested, blocked. Optional
per-task `reviews:` list records each coordinator assessment as
"r<N> <sent-back|accepted|escalated> <date> <notes>". The REVIEW_CAP-th
sent-back round is flagged as the final one: if that revision still isn't
right, the coordinator escalates to a human instead of sending it back again.

If PyYAML becomes a normal dependency, swap `load_yaml_subset` for
`yaml.safe_load` and delete the parser — everything below that point works
on plain dicts/lists and doesn't care how they were produced.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# --------------------------------------------------------------------------
# Minimal YAML-subset parser
# --------------------------------------------------------------------------

_KV_RE = re.compile(r"^(\w+):\s*(.*)$")


def _strip_comment(line: str) -> str:
    in_quote = None
    for idx, ch in enumerate(line):
        if in_quote:
            if ch == in_quote:
                in_quote = None
        elif ch in "\"'":
            in_quote = ch
        elif ch == "#":
            return line[:idx]
    return line


def _indent_of(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


def _parse_scalar(s: str):
    s = s.strip()
    if s == "":
        return None
    if s in ("true", "false"):
        return s == "true"
    if s.startswith("[") and s.endswith("]"):
        inner = s[1:-1].strip()
        if not inner:
            return []
        return [x.strip().strip("\"'") for x in inner.split(",")]
    if len(s) >= 2 and s[0] == s[-1] and s[0] in "\"'":
        return s[1:-1]
    return s


def load_yaml_subset(text: str) -> dict:
    lines = [_strip_comment(l.rstrip("\n")) for l in text.splitlines()]
    n = len(lines)

    def parse_scalar_list(start: int, dash_indent: int):
        items, cur, i = [], None, start
        while i < n:
            l = lines[i]
            if l.strip() == "":
                i += 1
                continue
            ind = _indent_of(l)
            body = l[ind:]
            if ind == dash_indent and body.startswith("- "):
                if cur is not None:
                    items.append(" ".join(cur).strip())
                cur = [body[2:].strip()]
                i += 1
            elif ind > dash_indent:
                cur.append(l.strip())
                i += 1
            else:
                break
        if cur is not None:
            items.append(" ".join(cur).strip())
        return items, i

    def parse_field(item: dict, content: str, field_indent: int, i: int):
        m = _KV_RE.match(content)
        if not m:
            return i
        key, val = m.group(1), m.group(2).strip()
        if val in (">", "|"):
            block, block_indent, i = [], None, i
            while i < n:
                l = lines[i]
                if l.strip() == "":
                    i += 1
                    continue
                ind = _indent_of(l)
                if ind <= field_indent:
                    break
                if block_indent is None:
                    block_indent = ind
                block.append(l[block_indent:])
                i += 1
            item[key] = " ".join(x.strip() for x in block).strip()
        elif val == "":
            j = i
            while j < n and lines[j].strip() == "":
                j += 1
            if j < n and _indent_of(lines[j]) > field_indent and lines[j].lstrip().startswith("- "):
                values, i = parse_scalar_list(j, _indent_of(lines[j]))
                item[key] = values
            else:
                item[key] = None
                i = j
        else:
            item[key] = _parse_scalar(val)
        return i

    def parse_mapping_item(i: int, indent: int):
        item = {}
        body = lines[i][indent:]
        assert body.startswith("- ")
        field_indent = indent + 2
        i = parse_field(item, body[2:], field_indent, i + 1)
        while i < n:
            l = lines[i]
            if l.strip() == "":
                i += 1
                continue
            ind = _indent_of(l)
            if ind != field_indent:
                break
            content = l[field_indent:]
            if content.startswith("- "):
                break
            i = parse_field(item, content, field_indent, i + 1)
        return item, i

    def parse_list(start: int, indent: int):
        items, i = [], start
        while i < n:
            l = lines[i]
            if l.strip() == "":
                i += 1
                continue
            if _indent_of(l) != indent or not l[indent:].startswith("- "):
                break
            item, i = parse_mapping_item(i, indent)
            items.append(item)
        return items, i

    result, i = {}, 0
    while i < n:
        l = lines[i]
        if l.strip() == "" or _indent_of(l) != 0:
            i += 1
            continue
        key, _, val = l.partition(":")
        key, val = key.strip(), val.strip()
        i += 1
        if val != "":
            result[key] = _parse_scalar(val)
            continue
        j = i
        while j < n and lines[j].strip() == "":
            j += 1
        if j < n and lines[j].lstrip().startswith("- "):
            items, i = parse_list(j, _indent_of(lines[j]))
            result[key] = items
        else:
            result[key] = None
    return result


# --------------------------------------------------------------------------
# Styling
# --------------------------------------------------------------------------

class C:
    RESET = "\033[0m"
    BOLD = "\033[1m"
    DIM = "\033[2m"
    GREEN = "\033[32m"
    CYAN = "\033[36m"
    YELLOW = "\033[33m"
    RED = "\033[31m"
    GRAY = "\033[90m"
    MAGENTA = "\033[35m"
    WHITE = "\033[97m"


STATUS_STYLE = {
    "tested": ("✓", C.GREEN, "tested"),
    "built": ("◆", C.CYAN, "built"),
    "in_progress": ("◐", C.YELLOW, "in_progress"),
    "in_revision": ("↻", C.MAGENTA, "in_revision"),
    "blocked": ("✗", C.RED, "blocked"),
    "pending": ("○", C.GRAY, "pending"),
}


def style(text: str, *codes: str, enabled: bool) -> str:
    if not enabled:
        return text
    return "".join(codes) + text + C.RESET


def bar(fraction: float, width: int, enabled: bool) -> str:
    filled = round(fraction * width)
    filled = max(0, min(width, filled))
    s = "█" * filled + "░" * (width - filled)
    if not enabled:
        return s
    color = C.GREEN if fraction >= 1 else (C.CYAN if fraction > 0 else C.GRAY)
    return color + s + C.RESET


def truncate(s: str, width: int) -> str:
    if len(s) <= width:
        return s
    if width <= 1:
        return s[:width]
    return s[: width - 1] + "…"


# --------------------------------------------------------------------------
# Rendering
# --------------------------------------------------------------------------

# A `reviews:` entry is the coordinator's assessment of one pass over a task:
# "r<N> <sent-back|accepted|escalated> <date> <free text>". Sent-back rounds
# are capped: after the REVIEW_CAP-th, the next step is a human, not a round.
REVIEW_RE = re.compile(r"^r(\d+)\s+(sent-back|accepted|escalated)\b")
REVIEW_CAP = 2


def render(data: dict, color: bool, width: int) -> str:
    phases = data.get("phases") or []
    tasks = data.get("tasks") or []
    by_id = {t["id"]: t for t in tasks}
    out = []

    def line(s: str = ""):
        out.append(s)

    def actionable(t: dict) -> bool:
        if t.get("status") != "pending":
            return False
        return all(by_id.get(d, {}).get("status") == "tested" for d in (t.get("depends_on") or []))

    def reviews(t: dict) -> list:
        r = t.get("reviews") or []
        return r if isinstance(r, list) else [str(r)]

    def sent_back_rounds(t: dict) -> int:
        return sum(1 for r in reviews(t) if (m := REVIEW_RE.match(r)) and m.group(2) == "sent-back")

    def last_verdict(t: dict):
        for r in reversed(reviews(t)):
            m = REVIEW_RE.match(r)
            if m:
                return m.group(2)
        return None

    def cap_hit(t: dict) -> bool:
        return sent_back_rounds(t) >= REVIEW_CAP and last_verdict(t) == "sent-back"

    def unmet_deps(t: dict):
        return [d for d in (t.get("depends_on") or []) if by_id.get(d, {}).get("status") != "tested"]

    # ---- header ----
    project = data.get("title", "PROJECT")
    title = f" {project} — Implementation Roadmap "
    inner_w = max(width - 2, len(title))
    line(style("╭" + "─" * inner_w + "╮", C.MAGENTA, enabled=color))
    line(style("│", C.MAGENTA, enabled=color) + style(title.center(inner_w), C.BOLD, C.WHITE, enabled=color) + style("│", C.MAGENTA, enabled=color))
    line(style("╰" + "─" * inner_w + "╯", C.MAGENTA, enabled=color))
    line()

    # ---- overall progress ----
    total = len(tasks)
    counts = {s: 0 for s in STATUS_STYLE}
    for t in tasks:
        counts[t.get("status", "pending")] = counts.get(t.get("status", "pending"), 0) + 1
    tested = counts.get("tested", 0)
    frac = (tested / total) if total else 0.0
    pct = round(frac * 100)

    bar_w = min(40, width - 30)
    summary_bits = "  ".join(
        style(f"{STATUS_STYLE[s][0]} {counts.get(s, 0)} {STATUS_STYLE[s][2]}", STATUS_STYLE[s][1], enabled=color)
        for s in ("tested", "built", "in_progress", "in_revision", "blocked", "pending")
    )
    line(style("Overall", C.BOLD, enabled=color) + f"  {bar(frac, bar_w, color)}  {tested}/{total} tested ({pct}%)")
    line(summary_bits)
    line()

    # ---- phases ----
    for ph in phases:
        ph_tasks = [t for t in tasks if t.get("phase") == ph["id"]]
        ph_total = len(ph_tasks)
        ph_tested = sum(1 for t in ph_tasks if t.get("status") == "tested")
        ph_frac = (ph_tested / ph_total) if ph_total else 0.0
        header = f"{ph.get('title', ph['id'])}"
        line(
            style(header, C.BOLD, enabled=color)
            + "  "
            + bar(ph_frac, 20, color)
            + f"  {ph_tested}/{ph_total}"
        )
        goal = ph.get("goal")
        if goal:
            line("  " + style(truncate(goal, width - 2), C.DIM, enabled=color))
        for t in ph_tasks:
            icon, col, _ = STATUS_STYLE.get(t.get("status", "pending"), STATUS_STYLE["pending"])
            mark = " ⚑" if t.get("milestone") else "  "
            tid = t["id"].ljust(24)
            tstatus = t.get("status", "pending").ljust(11)
            n_rev = len(reviews(t))
            badge = (style(f"r{n_rev}", C.RED if cap_hit(t) else C.MAGENTA, enabled=color) + " ") if n_rev else ""
            badge_w = len(f"r{n_rev} ") if n_rev else 0
            row = f"  {icon} " + style(tstatus, col, enabled=color) + " " + style(tid, C.DIM, enabled=color) + mark + " " + badge + truncate(t.get("title", ""), max(width - 46 - badge_w, 10))
            line(row)
        line()

    # ---- milestones ----
    milestones = [t for t in tasks if t.get("milestone")]
    if milestones:
        line(style("⚑ Milestones", C.BOLD, C.MAGENTA, enabled=color))
        for t in milestones:
            icon, col, _ = STATUS_STYLE.get(t.get("status", "pending"), STATUS_STYLE["pending"])
            line(f"  {icon} " + style(t.get("status", "pending").ljust(11), col, enabled=color) + " " + truncate(t.get("title", ""), width - 20))
        line()

    # ---- actionable now ("next", computed) ----
    next_tasks = [t for t in tasks if actionable(t)]
    line(style(f"▶ Actionable now ({len(next_tasks)})", C.BOLD, C.CYAN, enabled=color))
    if next_tasks:
        for t in next_tasks:
            line("  " + style(t["id"], C.CYAN, enabled=color) + "  " + truncate(t.get("title", ""), width - len(t["id"]) - 6))
    else:
        line(style("  (none — everything actionable is in progress, or nothing is unblocked)", C.DIM, enabled=color))
    line()

    # ---- in revision (sent back by the coordinator's review) ----
    revising = [t for t in tasks if t.get("status") == "in_revision"]
    line(style(f"↻ In revision ({len(revising)})", C.BOLD, C.MAGENTA, enabled=color))
    if revising:
        for t in revising:
            last = reviews(t)[-1] if reviews(t) else "no review recorded"
            warn = style(f"  ⚠ final round ({REVIEW_CAP}/{REVIEW_CAP}) — escalate if still not right", C.RED, enabled=color) if cap_hit(t) else ""
            line("  " + style(t["id"], C.MAGENTA, enabled=color) + warn + style(f"  — {truncate(last, max(width - len(t['id']) - 8, 20))}", C.DIM, enabled=color))
    else:
        line(style("  (none)", C.DIM, enabled=color))
    line()

    # ---- blocked ----
    blocked = [t for t in tasks if t.get("status") == "blocked"]
    line(style(f"⛔ Blocked ({len(blocked)})", C.BOLD, C.RED, enabled=color))
    if blocked:
        for t in blocked:
            reason = t.get("blocked_reason")
            if not reason:
                deps = unmet_deps(t)
                reason = ("waiting on: " + ", ".join(deps)) if deps else "no reason recorded"
            line("  " + style(t["id"], C.RED, enabled=color) + "  " + truncate(t.get("title", ""), 40) + style(f"  — {reason}", C.DIM, enabled=color))
    else:
        line(style("  (none)", C.DIM, enabled=color))

    return "\n".join(out)


# --------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------

def _default_tasks_file() -> Path | None:
    for candidate in (Path("docs/roadmap/tasks.yaml"), Path("tasks.yaml")):
        if candidate.exists():
            return candidate.resolve()
    return None


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--file",
        type=Path,
        default=None,
        help="Path to tasks.yaml (default: ./docs/roadmap/tasks.yaml or ./tasks.yaml, "
        "resolved against the current directory).",
    )
    parser.add_argument("--no-color", action="store_true", help="Disable ANSI colors.")
    parser.add_argument("--width", type=int, default=None, help="Override detected terminal width.")
    args = parser.parse_args(argv)

    file = args.file if args.file is not None else _default_tasks_file()
    if file is None:
        print(
            "error: no tasks.yaml found (tried ./docs/roadmap/tasks.yaml and "
            "./tasks.yaml) — pass --file",
            file=sys.stderr,
        )
        return 1
    if not file.exists():
        print(f"error: {file} not found", file=sys.stderr)
        return 1
    args.file = file

    data = load_yaml_subset(args.file.read_text())

    width = args.width
    if width is None:
        try:
            import shutil

            width = shutil.get_terminal_size(fallback=(96, 24)).columns
        except Exception:
            width = 96
    width = max(60, min(width, 140))

    color = (not args.no_color) and sys.stdout.isatty()

    print(render(data, color=color, width=width))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
