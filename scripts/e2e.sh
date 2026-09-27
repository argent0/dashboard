#!/usr/bin/env bash
# End-to-end scenario against the real `dashboard` binary and real tmux
# (spec.md, plan step 9a). Builds a throwaway director repo + hub in a temp
# dir, writes a small fake-agent script to stand in for a real Claude Code
# subagent, and drives the whole director/player/subagent loop by hand:
#
#   plan alpha -> post alpha -> tmux launch -> tested -> send-back -> tested
#   -> accept -> plan+post beta (depends on alpha) -> block -> escalated
#   -> retire -> a director rejection on main
#
# asserting each transition via `dashboard status --no-color` and raw git
# ref checks against the hub. See spec.md for the model and CLAUDE.md for
# the CLI/on-disk layout this drives.
#
# Usage: scripts/e2e.sh [path-to-dashboard-binary]
#   Defaults to target/release/dashboard (build it first: `cargo build --release`).
set -euo pipefail

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)

BIN_ARG=${1:-}
if [ -z "$BIN_ARG" ]; then
  BIN="$REPO_ROOT/target/release/dashboard"
else
  case "$BIN_ARG" in
    /*) BIN="$BIN_ARG" ;;
    *) BIN="$(pwd)/$BIN_ARG" ;;
  esac
fi
[ -x "$BIN" ] || { echo "e2e: dashboard binary not found or not executable: $BIN (build it with: cargo build --release)" >&2; exit 1; }
command -v tmux >/dev/null || { echo "e2e: tmux not found on PATH" >&2; exit 1; }
command -v git >/dev/null || { echo "e2e: git not found on PATH" >&2; exit 1; }

PROJECT="e2e-$$"
WORK=$(mktemp -d)
DIR_REPO="$WORK/director"
HOME_DIR="$WORK/home"
FAKE_AGENT="$WORK/fake-agent.sh"

TIMEOUT_SECS=60
POLL_SECS=0.5

log() { echo "[e2e] $*" >&2; }

cleanup() {
  local status=$?
  # Under `pipefail`, this whole pipeline reports failure if *any* stage
  # does (tmux list-sessions with no server running, or grep matching
  # nothing — both the common case once every session's already been
  # killed), which would otherwise abort this trap under `set -e` before
  # `rm -rf`/`exit "$status"` below ever run: guard the entire thing.
  { tmux list-sessions -F '#{session_name}' 2>/dev/null | grep -- "^dash-${PROJECT}-" | while IFS= read -r s; do
    tmux kill-session -t "$s" 2>/dev/null || true
  done; } || true
  rm -rf "$WORK"
  exit "$status"
}
trap cleanup EXIT

# --------------------------------------------------------------------------
# Small helpers
# --------------------------------------------------------------------------

dash() { "$BIN" --home "$HOME_DIR" "$@"; }
dash_status() { dash status --no-color; }

tick() {
  tick_capture >/dev/null
}

# Like `tick`, but also echoes the tick's report text to stdout so a caller
# can assert on a transition that a `dashboard status` snapshot might race
# past (see the send-back step: the fake agent polls continuously, so a
# send-back import and its round-trip finish can land in the very same
# tick, and `in_revision` is never independently observable).
tick_capture() {
  local out
  if ! out=$(dash player --once 2>&1); then
    echo "e2e: dashboard player --once failed:" >&2
    echo "$out" >&2
    exit 1
  fi
  if [ "$out" != "no changes" ]; then
    log "tick: $out"
  fi
  printf '%s' "$out"
}

# Ticks repeatedly (each iteration advances the player) until `"$@"`
# succeeds, sleeping POLL_SECS between attempts, up to TIMEOUT_SECS total —
# the "loop ticks with a short sleep until a condition holds" driver.
wait_until() {
  local desc=$1
  shift
  local deadline=$((SECONDS + TIMEOUT_SECS))
  while :; do
    tick
    if "$@"; then
      return 0
    fi
    if ((SECONDS >= deadline)); then
      echo "e2e: timed out waiting for: $desc" >&2
      echo "---- final status ----" >&2
      dash_status >&2 || true
      exit 1
    fi
    sleep "$POLL_SECS"
  done
}

# The status word for the live task row whose slug is `$1` (phase-block and
# in-flight rows both render "<icon> <status> <slug> ...", so the 3rd
# whitespace-separated field is the status and the 3rd... wait, 2nd is
# status and 3rd is slug: match on slug, print status). Retired tasks are
# excluded from these rows by design (spec §12) — checked separately via
# hub refs.
task_status_field() {
  dash_status | awk -v s="$1" '$3==s {print $2; exit}'
}
task_status_is() {
  [ "$(task_status_field "$1")" = "$2" ]
}

session_name() { echo "dash-${PROJECT}-$1"; }
session_alive() { tmux has-session -t "=$(session_name "$1")" 2>/dev/null; }
session_gone() { ! session_alive "$1"; }

hub_ref_exists() { git --git-dir "$HOME_DIR/hub.git" show-ref --verify --quiet "$1"; }
hub_main_body() { git --git-dir "$HOME_DIR/hub.git" log --first-parent -1 --format=%B refs/heads/main; }

assert_contains() {
  local haystack=$1 needle=$2 desc=$3
  if ! printf '%s' "$haystack" | grep -qF -- "$needle"; then
    echo "e2e: expected $desc to contain: $needle" >&2
    printf '%s\n' "$haystack" >&2
    exit 1
  fi
}

# --------------------------------------------------------------------------
# The fake agent: not an LLM, just a git-driven loop standing in for a real
# Claude Code subagent session (spec's launch row). It fetches from `origin`
# (the hub, per the player's subagent clone setup), fast-forwards onto any
# new director commit, then runs the director-authored `tasks/<slug>/
# agent*.sh` for the current review round and commits whatever it produced.
# --------------------------------------------------------------------------

write_fake_agent() {
  cat >"$FAKE_AGENT" <<'FAKE_AGENT_EOF'
#!/bin/sh
# Fake subagent driver (written by scripts/e2e.sh). $1 is the prompt text,
# ignored: the real instructions live in tasks/<slug>/agent*.sh, which this
# loop just executes. Never touches anything under tasks/<slug>/ besides
# reading it (those paths are pinned).
set -eu

git config user.name "E2E Fake Subagent" || true
git config user.email "fake-subagent@example.com" || true
git config commit.gpgsign false || true

end=$(($(date +%s) + 60))
while [ "$(date +%s)" -lt "$end" ]; do
  branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")
  case "$branch" in
    task/*) slug=${branch#task/} ;;
    *) slug="" ;;
  esac

  if [ -n "$slug" ]; then
    git fetch -q origin || true
    local_head=$(git rev-parse HEAD)
    remote_head=$(git rev-parse "origin/task/$slug" 2>/dev/null || echo "")
    if [ -n "$remote_head" ] && [ "$remote_head" != "$local_head" ] && git merge-base --is-ancestor "$local_head" "origin/task/$slug" 2>/dev/null; then
      git merge -q --ff-only "origin/task/$slug"
    fi

    # Already blocked: nothing more to do until the director retires this
    # task and kills the session.
    subject=$(git log -1 --format=%s 2>/dev/null || echo "")
    case "$subject" in
      block:*)
        sleep 0.5
        continue
        ;;
    esac

    # Highest review round N with tasks/<slug>/reviews/rN.md present at HEAD
    # picks agent-rN.sh; otherwise agent.sh (spec's send-back round).
    n=0
    for f in "tasks/$slug/reviews"/r*.md; do
      [ -e "$f" ] || continue
      base=$(basename "$f" .md)
      num=${base#r}
      case "$num" in
        *[!0-9]* | '') continue ;;
      esac
      if [ "$num" -gt "$n" ]; then
        n=$num
      fi
    done

    script="tasks/$slug/agent.sh"
    if [ "$n" -gt 0 ] && [ -f "tasks/$slug/agent-r$n.sh" ]; then
      script="tasks/$slug/agent-r$n.sh"
    fi

    if [ -f "$script" ]; then
      sh "$script" || true
      if [ -n "$(git status --porcelain)" ]; then
        git add -A
        git commit -q -m "agent: apply $(basename "$script")" || true
      fi
    fi
  fi

  sleep 0.5
done
FAKE_AGENT_EOF
  chmod +x "$FAKE_AGENT"
}

# --------------------------------------------------------------------------
# Setup: throwaway director repo + hub
# --------------------------------------------------------------------------

write_fake_agent

mkdir -p "$DIR_REPO"
git -C "$DIR_REPO" init -q -b main
git -C "$DIR_REPO" config user.name "E2E Director"
git -C "$DIR_REPO" config user.email "director@example.com"
git -C "$DIR_REPO" config commit.gpgsign false

cat >"$DIR_REPO/roadmap.toml" <<EOF
title = "E2E Project"

[[phases]]
id = "p1"
title = "Phase One"

[player]
agent_cmd = "$FAKE_AGENT"
EOF

cat >"$DIR_REPO/tests.toml" <<'EOF'
[[check]]
name = "baseline"
cmd = "true"
EOF

git -C "$DIR_REPO" add -A
git -C "$DIR_REPO" commit -q -m "initial roadmap and test suite"

log "director repo: $DIR_REPO"
log "dashboard home: $HOME_DIR"

init_out=$(cd "$DIR_REPO" && "$BIN" --home "$HOME_DIR" init --name "$PROJECT")
log "init: $init_out"

# --------------------------------------------------------------------------
# 1. Plan alpha: task/alpha from main with task.toml. Tick -> ready.
# --------------------------------------------------------------------------

log "step 1: plan alpha"
git -C "$DIR_REPO" checkout -q -b task/alpha main
mkdir -p "$DIR_REPO/tasks/alpha"
cat >"$DIR_REPO/tasks/alpha/task.toml" <<'EOF'
title = "Add feature"
phase = "p1"
EOF
git -C "$DIR_REPO" add -A
git -C "$DIR_REPO" commit -q -m "plan: alpha"
wait_until "task/alpha planned (ready)" task_status_is alpha ready

# --------------------------------------------------------------------------
# 2. Post alpha: prompt.md, agent.sh, the check added to tests.toml.
#    Tick -> launched in tmux. Tick until tested.
# --------------------------------------------------------------------------

log "step 2: post alpha"
cat >"$DIR_REPO/tasks/alpha/prompt.md" <<'EOF'
Implement the feature: create feature.txt.
EOF
cat >"$DIR_REPO/tasks/alpha/task.toml" <<'EOF'
title = "Add feature"
phase = "p1"

[check]
name = "alpha-check"
cmd = "test -f feature.txt"
EOF
cat >"$DIR_REPO/tasks/alpha/agent.sh" <<'EOF'
#!/bin/sh
echo ok > feature.txt
EOF
cat >"$DIR_REPO/tests.toml" <<'EOF'
[[check]]
name = "baseline"
cmd = "true"

[[check]]
name = "alpha-check"
cmd = "test -f feature.txt"
EOF
git -C "$DIR_REPO" add -A
git -C "$DIR_REPO" commit -q -m "post: alpha"

wait_until "task/alpha's tmux session exists" session_alive alpha
wait_until "task/alpha tested" task_status_is alpha tested

# --------------------------------------------------------------------------
# 3. `dashboard wait --timeout 5` prints the tested event.
# --------------------------------------------------------------------------

log "step 3: dashboard wait shows the tested event"
wait_out=$(dash wait --timeout 5)
assert_contains "$wait_out" "tested" "dashboard wait output"
assert_contains "$wait_out" "alpha" "dashboard wait output"

# --------------------------------------------------------------------------
# 4. Send back r1: reviews/r1.md, a new check in tests.toml, agent-r1.sh.
#    Tick -> in_revision. The fake agent pulls the review, runs agent-r1.sh,
#    and commits. Tick until tested again.
# --------------------------------------------------------------------------

log "step 4: send back alpha r1"
git -C "$DIR_REPO" fetch -q hub
git -C "$DIR_REPO" checkout -q task/alpha
git -C "$DIR_REPO" merge -q --ff-only hub/task/alpha

mkdir -p "$DIR_REPO/tasks/alpha/reviews"
cat >"$DIR_REPO/tasks/alpha/reviews/r1.md" <<'EOF'
Please also add feature2.txt.
EOF
cat >"$DIR_REPO/tasks/alpha/agent-r1.sh" <<'EOF'
#!/bin/sh
echo ok2 > feature2.txt
EOF
cat >"$DIR_REPO/tests.toml" <<'EOF'
[[check]]
name = "baseline"
cmd = "true"

[[check]]
name = "alpha-check"
cmd = "test -f feature.txt && test -f feature2.txt"
EOF
git -C "$DIR_REPO" add -A
git -C "$DIR_REPO" commit -q -m "send-back: alpha r1"

# A single tick imports the send-back (-> in_revision). But the fake agent
# has been polling every ~0.5s since launch, so it can fetch, run
# agent-r1.sh, and recommit before we ever look — landing back at `tested`
# within this same tick. in_revision is real but not reliably observable as
# a standalone `dashboard status` snapshot, so assert the transition
# happened via the tick's own report instead of polling for the status word.
send_back_out=$(tick_capture)
assert_contains "$send_back_out" "(send_back)" "the send-back tick's report"
wait_until "task/alpha tested again" task_status_is alpha tested

# --------------------------------------------------------------------------
# 5. Accept alpha. Tick -> hub main has a merge with Task: alpha, the tmux
#    session is gone, and status shows accepted.
# --------------------------------------------------------------------------

log "step 5: accept alpha"
git -C "$DIR_REPO" fetch -q hub
git -C "$DIR_REPO" checkout -q task/alpha
git -C "$DIR_REPO" merge -q --ff-only hub/task/alpha
git -C "$DIR_REPO" commit -q --allow-empty -m "accept: alpha"

wait_until "task/alpha accepted" task_status_is alpha accepted
assert_contains "$(hub_main_body)" "Task: alpha" "hub main's merge commit"
wait_until "alpha's tmux session is gone" session_gone alpha

# --------------------------------------------------------------------------
# 6. Plan and post beta (depends on alpha): the director must first fetch
#    hub and base beta on the new hub/main. Its agent.sh blocks.
#    Tick until escalated; `wait` shows an escalated event.
# --------------------------------------------------------------------------

log "step 6: plan and post beta (depends on alpha, then blocks)"
git -C "$DIR_REPO" fetch -q hub
git -C "$DIR_REPO" checkout -q main
git -C "$DIR_REPO" merge -q --ff-only hub/main
git -C "$DIR_REPO" checkout -q -b task/beta main
mkdir -p "$DIR_REPO/tasks/beta"
cat >"$DIR_REPO/tasks/beta/task.toml" <<'EOF'
title = "Second feature"
phase = "p1"
depends_on = ["alpha"]
EOF
git -C "$DIR_REPO" add -A
git -C "$DIR_REPO" commit -q -m "plan: beta"
wait_until "task/beta planned (ready)" task_status_is beta ready

cat >"$DIR_REPO/tasks/beta/prompt.md" <<'EOF'
Implement the second feature.
EOF
cat >"$DIR_REPO/tasks/beta/task.toml" <<'EOF'
title = "Second feature"
phase = "p1"
depends_on = ["alpha"]

[check]
name = "beta-check"
cmd = "test -f featureb.txt"
EOF
cat >"$DIR_REPO/tasks/beta/agent.sh" <<'EOF'
#!/bin/sh
git commit --allow-empty -m "block: stale test" -m "featureb.txt can't be produced as specified; the check needs a design change."
EOF
cat >"$DIR_REPO/tests.toml" <<'EOF'
[[check]]
name = "baseline"
cmd = "true"

[[check]]
name = "alpha-check"
cmd = "test -f feature.txt && test -f feature2.txt"

[[check]]
name = "beta-check"
cmd = "test -f featureb.txt"
EOF
git -C "$DIR_REPO" add -A
git -C "$DIR_REPO" commit -q -m "post: beta"

wait_until "task/beta's tmux session exists" session_alive beta
wait_until "task/beta escalated" task_status_is beta escalated

wait_out=$(dash wait --timeout 5)
assert_contains "$wait_out" "escalated" "dashboard wait output"
assert_contains "$wait_out" "beta" "dashboard wait output"

# --------------------------------------------------------------------------
# 7. Retire beta: retire: marker with a post-mortem body.
#    Tick -> refs/retired/beta exists and status shows retired with the
#    post-mortem.
# --------------------------------------------------------------------------

log "step 7: retire beta"
git -C "$DIR_REPO" fetch -q hub
git -C "$DIR_REPO" checkout -q task/beta
git -C "$DIR_REPO" merge -q --ff-only hub/task/beta
git -C "$DIR_REPO" commit -q --allow-empty -m "retire: beta" -m "The check as specified can't be met by the agent; retiring and will replan."

wait_until "refs/retired/beta exists" hub_ref_exists refs/retired/beta
assert_contains "$(dash_status)" "The check as specified can't be met" "status's retired post-mortem"
wait_until "beta's tmux session is gone" session_gone beta

# --------------------------------------------------------------------------
# 8. A director rejection path: commit a non-doc file on main ->
#    `dashboard wait` shows director_rejected.
# --------------------------------------------------------------------------

log "step 8: a non-doc commit on main is rejected"
git -C "$DIR_REPO" checkout -q main
echo "not a doc" >"$DIR_REPO/random.txt"
git -C "$DIR_REPO" add -A
git -C "$DIR_REPO" commit -q -m "oops: accidental non-doc commit on main"
tick

wait_out=$(dash wait --timeout 5)
assert_contains "$wait_out" "director_rejected" "dashboard wait output"

# --------------------------------------------------------------------------
# Final report
# --------------------------------------------------------------------------

echo
echo "==== final dashboard status ===="
dash_status
echo "================================"
echo
echo "e2e: PASS"
