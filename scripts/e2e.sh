#!/usr/bin/env bash
# End-to-end scenario against the real `dashboard` binary and real tmux
# (spec.md, plan step 9a). Builds a throwaway director repo + hub in a temp
# dir, writes a small fake-agent script to stand in for a real Claude Code
# worker, and drives the whole director/player/worker loop by hand:
#
#   plan alpha -> post alpha -> tmux launch -> tested -> send-back -> tested
#   -> accept -> plan+post beta (depends on alpha) -> block -> escalated
#   -> retire -> a spike (answer, send back, conclude) -> two tasks in
#   parallel, a conflicting accept, its resolution -> a worker that keeps
#   dying: relaunch, the relaunch cap, --reset -> task agents on: a
#   fake task agent posts and reviews a task in its own tmux session -> an
#   `arch check` suite check whose module view links a build cache -> a
#   director rejection on main
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
FAKE_TASK_AGENT="$WORK/fake-task-agent.sh"
TA_TEMPLATES="$WORK/ta-templates"
# What the fake worker saw: <slug>.prompts (every launch prompt, as `$1`
# arrived through tmux) and <slug>.pastes (everything pasted into its
# session). A <slug>.die file makes the worker crash at startup.
AGENT_LOGS="$WORK/agent-logs"

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

# How many `kind` events about `slug` the log holds (cursor untouched).
event_count() { dash wait --all | grep -cE "^#[0-9]+ $1 +$2( |$)" || true; }

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
# Claude Code worker session (spec's launch row). It fetches from `origin`
# (the hub, per the player's worker clone setup), fast-forwards onto any
# new director commit, then runs the director-authored `tasks/<slug>/
# agent*.sh` for the current review round and commits whatever it produced.
# --------------------------------------------------------------------------

write_fake_agent() {
  cat >"$FAKE_AGENT" <<'FAKE_AGENT_EOF'
#!/bin/sh
# Fake worker driver (written by scripts/e2e.sh). $1 is the prompt text,
# only logged: the real instructions live in tasks/<slug>/agent*.sh, which
# this loop just executes. Never touches anything under tasks/<slug>/ besides
# reading it (those paths are pinned).
set -eu
LOGS=__LOGS__

start_slug=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")
start_slug=${start_slug#task/}
printf '%s\n----\n' "$1" >>"$LOGS/$start_slug.prompts"
if [ -e "$LOGS/$start_slug.die" ]; then
  echo "fake worker: simulated crash at startup"
  sleep 1
  exit 3
fi
# Whatever the player pastes arrives on the pane's tty (our stdin; an
# asynchronous list would get /dev/null, hence fd 3).
exec 3<&0
cat <&3 >>"$LOGS/$start_slug.pastes" &
paster=$!
trap 'kill "$paster" 2>/dev/null || true' EXIT

git config user.name "E2E Fake Worker" || true
git config user.email "fake-worker@example.com" || true
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
  sed -i "s|__LOGS__|$AGENT_LOGS|" "$FAKE_AGENT"
  chmod +x "$FAKE_AGENT"
}

# --------------------------------------------------------------------------
# The fake task agent (spec §17): stands in for a real task agent session.
# Launched with "Step: post" in its prompt, it copies the post the scenario
# prepared for its slug ($TA_TEMPLATES/<slug>/) into its worktree, commits it
# on agent/<slug> and reports `posted`. Then it watches the hub: once the
# worker's work (<slug>.txt) is on task/<slug>, it reports `accept`.
# --------------------------------------------------------------------------

write_fake_task_agent() {
  cat >"$FAKE_TASK_AGENT" <<'FAKE_TASK_AGENT_EOF'
#!/bin/sh
# Fake task agent driver (written by scripts/e2e.sh). $1 is the prompt.
set -eu
TEMPLATES=__TEMPLATES__

git config user.name "E2E Fake Task Agent" || true
git config user.email "fake-task-agent@example.com" || true
git config commit.gpgsign false || true

branch=$(git rev-parse --abbrev-ref HEAD)
slug=${branch#agent/}
report="$(git rev-parse --absolute-git-dir)/dashboard-report.md"
write_report() {
  printf 'slug: %s\nstep: %s\ncommit: none\ncheck: %s.txt exists\nverdict: %s\nwhy: fake task agent.\nconcerns: none\n' \
    "$slug" "$1" "$slug" "$2" >"$report.tmp"
  mv "$report.tmp" "$report"
}

case "$1" in
  *"Step: post"*)
    git fetch -q origin
    git merge -q --ff-only "origin/task/$slug"
    cp -R "$TEMPLATES/$slug/." .
    git add -A
    git commit -q -m "post: $slug"
    write_report post posted
    ;;
esac

reviewed=""
end=$(($(date +%s) + 60))
while [ "$(date +%s)" -lt "$end" ]; do
  git fetch -q origin || true
  tip=$(git rev-parse "origin/task/$slug" 2>/dev/null || echo "")
  if [ -n "$tip" ] && [ "$tip" != "$reviewed" ] && git cat-file -e "$tip:$slug.txt" 2>/dev/null; then
    git merge -q --ff-only "origin/task/$slug" || true
    write_report "review r1" accept
    reviewed=$tip
  fi
  sleep 0.5
done
FAKE_TASK_AGENT_EOF
  sed -i "s|__TEMPLATES__|$TA_TEMPLATES|" "$FAKE_TASK_AGENT"
  chmod +x "$FAKE_TASK_AGENT"
}

# --------------------------------------------------------------------------
# Setup: throwaway director repo + hub
# --------------------------------------------------------------------------

mkdir -p "$AGENT_LOGS"
write_fake_agent
write_fake_task_agent

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
runs = "parallel"
task_agent_cmd = ""
EOF

cat >"$DIR_REPO/tests.toml" <<'EOF'
[[check]]
name = "baseline"
cmd = "true"
EOF

# Step 7c's suite check builds this cache in the checkout: ignored build
# output, which the jail's clean-worktree check allows.
echo "/cache/" >"$DIR_REPO/.gitignore"

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
# 2. Post alpha: prompt.md, agent.sh, the check in tests.d/alpha.toml.
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
mkdir -p "$DIR_REPO/tests.d"
cat >"$DIR_REPO/tests.d/alpha.toml" <<'EOF'
[[check]]
name = "alpha-check"
cmd = "test -f feature.txt"
EOF
git -C "$DIR_REPO" add -A
git -C "$DIR_REPO" commit -q -m "post: alpha"

wait_until "task/alpha's tmux session exists" session_alive alpha
wait_until "task/alpha tested" task_status_is alpha tested
alpha_prompt=$(cat "$AGENT_LOGS/alpha.prompts")
assert_contains "$alpha_prompt" "You are the worker for task \`alpha\` in project \`$PROJECT\`" "alpha's launch prompt"
assert_contains "$alpha_prompt" "tests.d/alpha.toml" "alpha's launch prompt (its pinned files)"
assert_contains "$alpha_prompt" "Implement the feature: create feature.txt." "alpha's launch prompt (prompt.md)"

# --------------------------------------------------------------------------
# 3. `dashboard wait --timeout 5` prints the tested event.
# --------------------------------------------------------------------------

log "step 3: dashboard wait shows the tested event"
wait_out=$(dash wait --timeout 5)
assert_contains "$wait_out" "tested" "dashboard wait output"
assert_contains "$wait_out" "alpha" "dashboard wait output"

# --------------------------------------------------------------------------
# 4. Send back r1: reviews/r1.md, a tighter check in tests.d/alpha.toml, agent-r1.sh.
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
cat >"$DIR_REPO/tests.d/alpha.toml" <<'EOF'
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
alpha_pasted() { grep -qF "Please also add feature2.txt." "$AGENT_LOGS/alpha.pastes" 2>/dev/null; }
wait_until "the review pasted into alpha's session" alpha_pasted
alpha_pastes=$(cat "$AGENT_LOGS/alpha.pastes")
assert_contains "$alpha_pastes" "finish accepted" "alpha's pasted relays"
assert_contains "$alpha_pastes" "Review round 1 from the director" "alpha's pasted relays"

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
cat >"$DIR_REPO/tests.d/beta.toml" <<'EOF'
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
# 7b. A spike (spec §14): plan + post a question, the fake agent answers it
#     with findings and claims -> tested; send back with a refined question
#     -> answered again; conclude -> explored. The branch moves to
#     refs/explored/probe and main never moves.
# --------------------------------------------------------------------------

log "step 7b: spike probe (plan, post, answer, send back, conclude)"
main_before=$(git --git-dir "$HOME_DIR/hub.git" rev-parse refs/heads/main)
git -C "$DIR_REPO" checkout -q -b task/probe main
mkdir -p "$DIR_REPO/tasks/probe"
cat >"$DIR_REPO/tasks/probe/task.toml" <<'EOF'
kind = "spike"
title = "Can we build the spike feature?"
phase = "p1"
EOF
git -C "$DIR_REPO" add -A
git -C "$DIR_REPO" commit -q -m "plan: probe"
wait_until "spike probe planned (ready)" task_status_is probe ready

mkdir -p "$DIR_REPO/tasks/probe/questions" "$DIR_REPO/tasks/probe/baseline"
cat >"$DIR_REPO/tasks/probe/prompt.md" <<'EOF'
Find out whether spikefeature.txt can be produced.
EOF
cat >"$DIR_REPO/tasks/probe/questions/1.toml" <<'EOF'
question = "Can we produce spikefeature.txt?"
why = "decides the design"
answers = ["yes", "no"]
EOF
cat >"$DIR_REPO/tasks/probe/baseline/1.toml" <<'EOF'
[[claim]]
name = "no-spike-feature-yet"
cmd = "test ! -f spikefeature.txt"
EOF
cat >"$DIR_REPO/tasks/probe/agent.sh" <<'EOF'
#!/bin/sh
# Answers whichever round is current: the number of question files.
n=$(ls tasks/probe/questions | wc -l)
echo built > spikefeature.txt
mkdir -p tasks/probe/findings tasks/probe/claims
echo "It works. Recommend building it." > "tasks/probe/findings/$n.md"
cat > "tasks/probe/claims/$n.toml" <<CLAIMS_EOF
answer = "yes"

[[claim]]
name = "has-spike-feature"
kind = "spike"
supports = "yes"
cmd = "test -f spikefeature.txt"
CLAIMS_EOF
EOF
git -C "$DIR_REPO" add -A
git -C "$DIR_REPO" commit -q -m "post: probe"

wait_until "spike probe's tmux session exists" session_alive probe
wait_until "spike probe tested" task_status_is probe tested
assert_contains "$(dash_status)" "round 1, latest answer: yes" "status's spike line"

log "step 7b: send the spike back with a sharper question"
git -C "$DIR_REPO" fetch -q hub
git -C "$DIR_REPO" checkout -q task/probe
git -C "$DIR_REPO" merge -q --ff-only hub/task/probe
cat >"$DIR_REPO/tasks/probe/questions/2.toml" <<'EOF'
question = "Can we still produce it with the cache cold?"
answers = ["yes", "no"]
refines = 1
EOF
git -C "$DIR_REPO" add -A
git -C "$DIR_REPO" commit -q -m "send-back: probe round 2"
send_back_out=$(tick_capture)
assert_contains "$send_back_out" "(send_back)" "the spike send-back tick's report"
wait_until "spike probe answered round 2" task_status_is probe tested
assert_contains "$(dash_status)" "round 2, latest answer: yes" "status's spike line after round 2"

log "step 7b: conclude the spike"
git -C "$DIR_REPO" fetch -q hub
git -C "$DIR_REPO" checkout -q task/probe
git -C "$DIR_REPO" merge -q --ff-only hub/task/probe
git -C "$DIR_REPO" commit -q --allow-empty -m "conclude: yes" -m "The operator agreed with the answer."
wait_until "spike probe explored" task_status_is probe explored
assert_contains "$(dash_status)" "answer: yes (round 2)" "status's explored spike"
hub_ref_exists refs/explored/probe || { echo "e2e: refs/explored/probe missing" >&2; exit 1; }
if hub_ref_exists refs/heads/task/probe; then echo "e2e: refs/heads/task/probe should have moved to refs/explored/probe" >&2; exit 1; fi
[ "$(git --git-dir "$HOME_DIR/hub.git" rev-parse refs/heads/main)" = "$main_before" ] || { echo "e2e: a spike must never move main" >&2; exit 1; }
wait_until "probe's tmux session is gone" session_gone probe

# --------------------------------------------------------------------------
# 7c. Parallel tasks (spec §4): gamma and delta posted side by side, both
#     workers live at once. Each post adds its own tests.d/<slug>.toml, so
#     the suite never conflicts; but both agents write shared.txt
#     differently, so once gamma is accepted, the player can't merge main
#     into delta to land its accept:
#     delta goes `conflicted`, and the director resolves exactly that
#     conflict (merging the recorded main commit), which lands the accept.
# --------------------------------------------------------------------------

log "step 7c: two tasks in parallel, a conflict, a targeted resolution"
git -C "$DIR_REPO" fetch -q hub
git -C "$DIR_REPO" checkout -q main
git -C "$DIR_REPO" merge -q --ff-only hub/main
for t in gamma delta; do
  git -C "$DIR_REPO" checkout -q -b "task/$t" main
  mkdir -p "$DIR_REPO/tasks/$t"
  printf 'title = "%s"\nphase = "p1"\n' "$t" >"$DIR_REPO/tasks/$t/task.toml"
  git -C "$DIR_REPO" add -A
  git -C "$DIR_REPO" commit -q -m "plan: $t"
done
both_ready() { task_status_is gamma ready && task_status_is delta ready; }
both_alive() { session_alive gamma && session_alive delta; }
wait_until "gamma and delta planned" both_ready

for t in gamma delta; do
  git -C "$DIR_REPO" checkout -q "task/$t"
  echo "Create $t.txt." >"$DIR_REPO/tasks/$t/prompt.md"
  printf 'title = "%s"\nphase = "p1"\n\n[check]\nname = "%s-check"\ncmd = "test -f %s.txt"\n' "$t" "$t" "$t" >"$DIR_REPO/tasks/$t/task.toml"
  printf '#!/bin/sh\necho ok > %s.txt\necho %s > shared.txt\n' "$t" "$t" >"$DIR_REPO/tasks/$t/agent.sh"
  printf '[[check]]\nname = "%s-check"\ncmd = "test -f %s.txt"\n' "$t" "$t" >"$DIR_REPO/tests.d/$t.toml"
  git -C "$DIR_REPO" add -A
  git -C "$DIR_REPO" commit -q -m "post: $t"
done
wait_until "gamma's and delta's sessions both alive" both_alive
wait_until "task/gamma tested" task_status_is gamma tested
wait_until "task/delta tested" task_status_is delta tested

git -C "$DIR_REPO" fetch -q hub
git -C "$DIR_REPO" checkout -q task/gamma
git -C "$DIR_REPO" merge -q --ff-only hub/task/gamma
git -C "$DIR_REPO" commit -q --allow-empty -m "accept: gamma"
wait_until "task/gamma accepted" task_status_is gamma accepted

git -C "$DIR_REPO" fetch -q hub
git -C "$DIR_REPO" checkout -q task/delta
git -C "$DIR_REPO" merge -q --ff-only hub/task/delta
main_at_conflict=$(git --git-dir "$HOME_DIR/hub.git" rev-parse refs/heads/main)
git -C "$DIR_REPO" commit -q --allow-empty -m "accept: delta"
wait_until "task/delta conflicted" task_status_is delta conflicted
wait_out=$(dash wait --timeout 5)
assert_contains "$wait_out" "conflict" "dashboard wait output"
assert_contains "$wait_out" "$main_at_conflict" "the conflict event names the main commit"

# The resolution merges exactly that main commit and touches only shared.txt.
git -C "$DIR_REPO" fetch -q hub
git -C "$DIR_REPO" merge -q --ff-only hub/task/delta
if git -C "$DIR_REPO" merge -q --no-edit "$main_at_conflict" >/dev/null 2>&1; then
  echo "e2e: expected shared.txt to conflict when resolving delta" >&2
  exit 1
fi
printf 'gamma\ndelta\n' >"$DIR_REPO/shared.txt"
git -C "$DIR_REPO" add -A
git -C "$DIR_REPO" commit -q --no-edit
wait_until "task/delta accepted" task_status_is delta accepted
assert_contains "$(hub_main_body)" "Task: delta" "hub main's merge commit"
for t in gamma delta; do
  git --git-dir "$HOME_DIR/hub.git" cat-file -e "refs/heads/main:tests.d/$t.toml" || { echo "e2e: tests.d/$t.toml missing on main" >&2; exit 1; }
done

# --------------------------------------------------------------------------
# 7d. A worker that keeps dying (eta): each death is a session_died event
#     with its exit status and last screen; `dashboard relaunch` (its own
#     process) restarts it up to max_relaunches; one more is refused and
#     escalates; once the cause is gone, `relaunch --reset` clears that
#     escalation (a `resumed` event) and the task goes on to be accepted.
# --------------------------------------------------------------------------

log "step 7d: a dying worker, relaunch, the cap, --reset (eta)"
touch "$AGENT_LOGS/eta.die"
git -C "$DIR_REPO" checkout -q main
git -C "$DIR_REPO" fetch -q hub
git -C "$DIR_REPO" merge -q --ff-only hub/main
git -C "$DIR_REPO" checkout -q -b task/eta main
mkdir -p "$DIR_REPO/tasks/eta"
printf 'title = "eta"\nphase = "p1"\n' >"$DIR_REPO/tasks/eta/task.toml"
git -C "$DIR_REPO" add -A
git -C "$DIR_REPO" commit -q -m "plan: eta"
wait_until "eta is ready" task_status_is eta ready
echo "Create eta.txt." >"$DIR_REPO/tasks/eta/prompt.md"
printf 'title = "eta"\nphase = "p1"\n\n[check]\nname = "eta-check"\ncmd = "test -f eta.txt"\n' >"$DIR_REPO/tasks/eta/task.toml"
printf '#!/bin/sh\necho ok > eta.txt\n' >"$DIR_REPO/tasks/eta/agent.sh"
printf '[[check]]\nname = "eta-check"\ncmd = "test -f eta.txt"\n' >"$DIR_REPO/tests.d/eta.toml"
git -C "$DIR_REPO" add -A
git -C "$DIR_REPO" commit -q -m "post: eta"

eta_died() { [ "$(event_count session_died eta)" -ge "$1" ] && session_gone eta; }
wait_until "eta's worker died" eta_died 1
died_out=$(dash wait --all | grep -A30 -E "^#[0-9]+ session_died +eta")
assert_contains "$died_out" "exit status 3" "eta's session_died event"
assert_contains "$died_out" "fake worker: simulated crash at startup" "eta's session_died event (last screen)"

for n in 1 2; do
  relaunch_out=$(dash relaunch eta)
  assert_contains "$relaunch_out" "eta" "dashboard relaunch output"
  wait_until "eta's worker died again (relaunch $n)" eta_died $((n + 1))
  assert_contains "$(dash_status)" "relaunched $n/2" "dashboard status"
done
if dash relaunch eta >/dev/null 2>&1; then
  echo "e2e: a relaunch past max_relaunches was not refused" >&2
  exit 1
fi
wait_until "eta escalated by the relaunch cap" task_status_is eta escalated
hub_ref_exists refs/dashboard/escalated/eta || { echo "e2e: no escalation ref for eta" >&2; exit 1; }
[ "$(grep -c '^----$' "$AGENT_LOGS/eta.prompts")" -eq 3 ] || { echo "e2e: eta's worker should have launched 3 times" >&2; exit 1; }

# The operator fixes the cause; --reset clears that escalation and relaunches.
rm "$AGENT_LOGS/eta.die"
dash relaunch eta --reset >/dev/null
eta_resumed() { [ "$(event_count resumed eta)" -ge 1 ]; }
wait_until "eta resumed" eta_resumed
wait_until "task/eta tested" task_status_is eta tested
hub_ref_exists refs/dashboard/escalated/eta && { echo "e2e: eta's escalation ref survived --reset" >&2; exit 1; }
git -C "$DIR_REPO" fetch -q hub
git -C "$DIR_REPO" merge -q --ff-only hub/task/eta
git -C "$DIR_REPO" commit -q --allow-empty -m "accept: eta"
wait_until "task/eta accepted" task_status_is eta accepted

# --------------------------------------------------------------------------
# 7e. Task agents (spec §17): a roadmap commit on main turns them on; the
#     player launches a fake task agent for the newly planned epsilon in
#     its own tmux session; it posts (imported as the task agent's), the
#     worker finishes, the task agent reports accept, the director accepts
#     on it, and the task agent's session ends with the task.
# --------------------------------------------------------------------------

log "step 7e: a task agent posts and reviews epsilon"
git -C "$DIR_REPO" checkout -q main
git -C "$DIR_REPO" fetch -q hub
git -C "$DIR_REPO" merge -q --ff-only hub/main
sed -i "s|^task_agent_cmd = \"\"|task_agent_cmd = \"$FAKE_TASK_AGENT\"|" "$DIR_REPO/roadmap.toml"
git -C "$DIR_REPO" commit -q -am "roadmap: turn task agents on"
hub_has_task_agents() { git --git-dir "$HOME_DIR/hub.git" show refs/heads/main:roadmap.toml | grep -qF "$FAKE_TASK_AGENT"; }
wait_until "roadmap with task agents on main" hub_has_task_agents

mkdir -p "$TA_TEMPLATES/epsilon/tasks/epsilon" "$TA_TEMPLATES/epsilon/tests.d"
echo "Create epsilon.txt. Tests you add must be pure." >"$TA_TEMPLATES/epsilon/tasks/epsilon/prompt.md"
printf '#!/bin/sh\necho ok > epsilon.txt\n' >"$TA_TEMPLATES/epsilon/tasks/epsilon/agent.sh"
printf '[[check]]\nname = "epsilon-check"\ncmd = "test -f epsilon.txt"\n' >"$TA_TEMPLATES/epsilon/tests.d/epsilon.toml"

git -C "$DIR_REPO" checkout -q -b task/epsilon main
mkdir -p "$DIR_REPO/tasks/epsilon"
printf 'title = "epsilon"\nphase = "p1"\nbrief = "a fake task agent posts this"\n\n[check]\nname = "epsilon-check"\ncmd = "test -f epsilon.txt"\n' >"$DIR_REPO/tasks/epsilon/task.toml"
git -C "$DIR_REPO" add -A
git -C "$DIR_REPO" commit -q -m "plan: epsilon"

# A ready task never starts on its own: it waits for the director's `stage:`.
wait_until "epsilon is ready" task_status_is epsilon ready
tick; tick
session_gone epsilon_agent || { echo "e2e: a ready task's task agent started without a stage:" >&2; exit 1; }
git -C "$DIR_REPO" fetch -q hub
git -C "$DIR_REPO" merge -q --ff-only hub/task/epsilon
git -C "$DIR_REPO" commit -q --allow-empty -m "stage: epsilon" -m "start its task agent"
wait_until "epsilon is staged" task_status_is epsilon staged
wait_until "epsilon's task agent session exists" session_alive epsilon_agent
wait_until "epsilon's worker session exists" session_alive epsilon
post_sha=$(git --git-dir "$HOME_DIR/hub.git" log --format=%H --grep='^post: epsilon' refs/heads/task/epsilon)
assert_contains "$(git --git-dir "$HOME_DIR/hub.git" notes --ref refs/notes/dashboard-imports show "$post_sha")" '"actor":"task_agent"' "epsilon's post import note"

agent_recommends_accept() { dash_status | grep -qF "reported: accept"; }
wait_until "epsilon's task agent recommends accept" agent_recommends_accept
assert_contains "$(dash_status)" "task agent: dash-${PROJECT}-epsilon_agent (alive)" "dashboard status"
wait_out=$(dash wait --timeout 5)
assert_contains "$wait_out" "agent_launched" "dashboard wait output"
assert_contains "$wait_out" "verdict: accept" "dashboard wait output"
ask_out=$(dash ask epsilon "why that check?")
assert_contains "$ask_out" "asked task/epsilon's task agent" "dashboard ask output"

git -C "$DIR_REPO" fetch -q hub
git -C "$DIR_REPO" merge -q --ff-only hub/task/epsilon
git -C "$DIR_REPO" commit -q --allow-empty -m "accept: epsilon" -m "the task agent recommends it"
wait_until "task/epsilon accepted" task_status_is epsilon accepted
wait_until "epsilon's task agent session ended" session_gone epsilon_agent

# --------------------------------------------------------------------------
# 7f. Module views link build caches: the director commits an
#     architecture.toml with `linked = ["cache"]`; zeta's post adds a suite
#     check that builds cache/ and runs `dashboard arch check` (in the jail),
#     whose module test_cmd only passes if its view links the cache and has
#     DASHBOARD_MODULE set. Failing at post (no zeta/lib.rs), passing once the
#     worker writes it, accepted.
# --------------------------------------------------------------------------

log "step 7f: arch check views link a build cache (zeta)"
git -C "$DIR_REPO" checkout -q main
git -C "$DIR_REPO" fetch -q hub
git -C "$DIR_REPO" merge -q --ff-only hub/main
cat >"$DIR_REPO/architecture.toml" <<'EOF'
max_modules = 1
linked = ["cache"]

[[module]]
name = "zeta"
lang = "rust"
paths = ["zeta"]
max_ast = 100000
max_bytes = 100000
test_cmd = 'test -L cache && test "$(cat cache/built)" = ok && test "$DASHBOARD_MODULE" = zeta && test -f zeta/lib.rs'
EOF
git -C "$DIR_REPO" add architecture.toml
git -C "$DIR_REPO" commit -q -m "architecture: one module, a linked cache"
hub_has_arch() { git --git-dir "$HOME_DIR/hub.git" cat-file -e refs/heads/main:architecture.toml 2>/dev/null; }
wait_until "architecture.toml on main" hub_has_arch

arch_cmd="mkdir -p cache && echo ok > cache/built && $BIN arch check --root ."
mkdir -p "$TA_TEMPLATES/zeta/tasks/zeta" "$TA_TEMPLATES/zeta/tests.d"
echo "Create zeta/lib.rs and zeta.txt." >"$TA_TEMPLATES/zeta/tasks/zeta/prompt.md"
printf '#!/bin/sh\nmkdir -p zeta\necho "pub fn z() {}" > zeta/lib.rs\necho ok > zeta.txt\n' >"$TA_TEMPLATES/zeta/tasks/zeta/agent.sh"
printf '[[check]]\nname = "zeta-arch"\ncmd = "%s"\n' "$arch_cmd" >"$TA_TEMPLATES/zeta/tests.d/zeta.toml"

git -C "$DIR_REPO" checkout -q -b task/zeta main
mkdir -p "$DIR_REPO/tasks/zeta"
printf 'title = "zeta"\nphase = "p1"\nbrief = "a module test in a view with a linked cache"\n\n[check]\nname = "zeta-arch"\ncmd = "%s"\n' "$arch_cmd" >"$DIR_REPO/tasks/zeta/task.toml"
git -C "$DIR_REPO" add -A
git -C "$DIR_REPO" commit -q -m "plan: zeta"
wait_until "zeta is ready" task_status_is zeta ready
git -C "$DIR_REPO" fetch -q hub
git -C "$DIR_REPO" merge -q --ff-only hub/task/zeta
git -C "$DIR_REPO" commit -q --allow-empty -m "stage: zeta" -m "start its task agent"
wait_until "zeta's worker session exists" session_alive zeta
wait_until "task/zeta tested" task_status_is zeta tested

git -C "$DIR_REPO" fetch -q hub
git -C "$DIR_REPO" merge -q --ff-only hub/task/zeta
git -C "$DIR_REPO" commit -q --allow-empty -m "accept: zeta" -m "arch check passes in the view"
wait_until "task/zeta accepted" task_status_is zeta accepted
git --git-dir "$HOME_DIR/hub.git" cat-file -e refs/heads/main:zeta/lib.rs || { echo "e2e: zeta/lib.rs missing on main" >&2; exit 1; }
if git --git-dir "$HOME_DIR/hub.git" cat-file -e refs/heads/main:cache/built 2>/dev/null; then
  echo "e2e: the linked cache was committed to main" >&2; exit 1
fi

# --------------------------------------------------------------------------
# 8. A director rejection path: commit a non-doc file on main ->
#    `dashboard wait` shows director_rejected.
# --------------------------------------------------------------------------

log "step 8: a non-doc commit on main is rejected"
git -C "$DIR_REPO" checkout -q main
git -C "$DIR_REPO" fetch -q hub
git -C "$DIR_REPO" merge -q --ff-only hub/main
echo "not a doc" >"$DIR_REPO/random.txt"
git -C "$DIR_REPO" add -A
git -C "$DIR_REPO" commit -q -m "oops: accidental non-doc commit on main"
tick

wait_out=$(dash wait --timeout 5)
assert_contains "$wait_out" "director_rejected" "dashboard wait output"
assert_contains "$wait_out" "ownership" "dashboard wait output"

# --------------------------------------------------------------------------
# Final report
# --------------------------------------------------------------------------

echo
echo "==== final dashboard status ===="
dash_status
echo "================================"
echo
echo "e2e: PASS"
