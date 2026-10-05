// Integration tests for spikes (spec.md §14): the `spike` task kind whose
// product is an answer to a pinned question, backed by claims the player runs
// itself, and which never merges. Real director repos, real hubs, real
// worker worktrees (as in tests/player_worker.rs); `NullSessions` stands
// in for tmux, and the worker is simulated by committing in the clone the
// player created.

mod common;

use common::*;
use dashboard::config::Reason;
use dashboard::git::Git;
use dashboard::init;
use dashboard::paths::{self, Home};
use dashboard::player::{NullSessions, Player, TickReport};
use dashboard::records::{self, Transition};
use dashboard::render;
use dashboard::state::{self, Status};
use tempfile::TempDir;

// --------------------------------------------------------------------------
// Fixtures
// --------------------------------------------------------------------------

const QUESTION: &str = "question = \"Can we produce feature.txt?\"\nwhy = \"decides the design\"\nanswers = [\"yes\", \"no\"]\n";

fn init_hub(repo: &DirectorRepo) -> (TempDir, Home) {
    let home_dir = temp_home();
    let report = init::init(
        repo.path(),
        Some("proj"),
        Some(home_dir.path().join("home")),
    )
    .expect("init should succeed");
    (home_dir, Home { root: report.home })
}

fn open_player(home: &Home) -> Player<NullSessions> {
    Player::open(
        Home {
            root: home.root.clone(),
        },
        NullSessions::default(),
    )
    .expect("player open")
}

fn status_of(hub: &Git, slug: &str) -> Status {
    let hub_state = state::derive(hub, &|_| false).expect("derive should succeed");
    hub_state
        .tasks
        .iter()
        .find(|t| t.slug == slug)
        .unwrap_or_else(|| panic!("no task state for {slug:?}"))
        .status
}

fn strikes(hub: &Git, slug: &str, reason: Reason) -> u32 {
    let hub_state = state::derive(hub, &|_| false).expect("derive should succeed");
    hub_state
        .tasks
        .iter()
        .find(|t| t.slug == slug)
        .and_then(|t| t.strikes.get(&reason).copied())
        .unwrap_or(0)
}

fn plan_spike(repo: &DirectorRepo, player: &mut Player<NullSessions>, slug: &str) {
    let toml = format!("kind = \"spike\"\ntitle = \"{slug} spike\"\nphase = \"p1\"\n");
    director_commit_on_branch(
        repo.path(),
        &format!("task/{slug}"),
        &format!("tasks/{slug}/task.toml"),
        &toml,
        &format!("plan: {slug}"),
    );
    let report = player.tick().expect("plan tick");
    assert!(
        report.rejected.is_empty(),
        "plan: {slug} unexpectedly rejected: {report}"
    );
}

/// The director's post for a spike: prompt, round-1 question, and the given
/// extra `(path, content)` files (baseline claims and their scripts).
fn post_spike_with(
    repo: &DirectorRepo,
    player: &mut Player<NullSessions>,
    slug: &str,
    extra: &[(&str, &str)],
) -> TickReport {
    checkout_branch(repo.path(), &format!("task/{slug}"));
    write_file(
        repo.path(),
        &format!("tasks/{slug}/prompt.md"),
        "find out\n",
    );
    write_file(
        repo.path(),
        &format!("tasks/{slug}/questions/1.toml"),
        QUESTION,
    );
    for (rel, content) in extra {
        write_file(repo.path(), rel, content);
    }
    commit(repo.path(), &format!("post: {slug}"));
    player.tick().expect("post tick")
}

/// A baseline claim that holds on the current code: no `feature.txt` yet.
fn baseline_file(slug: &str) -> (String, String) {
    (
        format!("tasks/{slug}/baseline/1.toml"),
        "[[claim]]\nname = \"no-feature-yet\"\ncmd = \"test ! -f feature.txt\"\n".to_string(),
    )
}

fn plan_and_post(repo: &DirectorRepo, player: &mut Player<NullSessions>, slug: &str) -> TickReport {
    plan_spike(repo, player, slug);
    let (bpath, bcontent) = baseline_file(slug);
    let report = post_spike_with(repo, player, slug, &[(&bpath, &bcontent)]);
    assert_eq!(report.launched, vec![slug.to_string()], "{report}");
    assert!(report.errors.is_empty(), "{report}");
    report
}

/// Simulates the worker's commit in the clone the player made.
fn commit_in_clone(repo: &DirectorRepo, slug: &str, files: &[(&str, &str)], msg: &str) -> String {
    let dir = paths::worker_clone(&repo.director_path(), slug);
    let git = Git::new(&dir);
    git.run(&["config", "user.name", "Test Worker"]).unwrap();
    git.run(&["config", "user.email", "worker@example.com"])
        .unwrap();
    git.run(&["config", "commit.gpgsign", "false"]).unwrap();
    for (rel, content) in files {
        write_file(&dir, rel, content);
    }
    commit(&dir, msg)
}

/// What the worker does on a relayed send-back: fetch the hub's tip and
/// fast-forward onto it before committing again.
fn pull_in_clone(repo: &DirectorRepo, slug: &str) {
    let dir = paths::worker_clone(&repo.director_path(), slug);
    let git = Git::new(&dir);
    git.run(&["fetch", "-q", "origin"])
        .expect("fetch from the hub");
    git.run(&["merge", "-q", "--ff-only", &format!("origin/task/{slug}")])
        .expect("fast-forward onto the hub's task branch");
}

fn claims_toml(answer: &str, body: &str) -> String {
    format!("answer = \"{answer}\"\n\n{body}")
}

/// A spike claim that passes only where `feature.txt` exists.
fn spike_claim(name: &str, supports: &str) -> String {
    format!(
        "[[claim]]\nname = \"{name}\"\nkind = \"spike\"\nsupports = \"{supports}\"\ncmd = \"test -f feature.txt\"\n"
    )
}

/// The files of a well-formed round-1 finish: the work, findings, claims.
fn good_finish_files(slug: &str) -> Vec<(String, String)> {
    vec![
        ("feature.txt".to_string(), "built\n".to_string()),
        (
            format!("tasks/{slug}/findings/1.md"),
            "It works; recommend X.\n".to_string(),
        ),
        (
            format!("tasks/{slug}/claims/1.toml"),
            claims_toml("yes", &spike_claim("has-feature", "yes")),
        ),
    ]
}

fn finish_with(
    repo: &DirectorRepo,
    player: &mut Player<NullSessions>,
    slug: &str,
    files: &[(String, String)],
) -> TickReport {
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    commit_in_clone(repo, slug, &refs, "finish");
    player.tick().expect("finish tick")
}

fn one_rejection(report: &TickReport, reason: Reason) -> String {
    assert_eq!(
        report.rejected.len(),
        1,
        "expected exactly one rejection: {report}"
    );
    assert_eq!(report.rejected[0].reason, reason, "{report}");
    report.rejected[0].message.clone()
}

// --------------------------------------------------------------------------
// Plan
// --------------------------------------------------------------------------

#[test]
fn a_spike_is_planned_ready_and_never_depends_on_anything() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan_spike(&repo, &mut player, "probe");
    assert_eq!(status_of(&hub, "probe"), Status::Ready);

    director_commit_on_branch(
        repo.path(),
        "task/bad",
        "tasks/bad/task.toml",
        "kind = \"spike\"\ndepends_on = [\"probe\"]\n",
        "plan: bad",
    );
    let report = player.tick().unwrap();
    assert!(
        one_rejection(&report, Reason::Plan).contains("spike can't depend"),
        "{report}"
    );
}

#[test]
fn nothing_may_depend_on_a_spike() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let mut player = open_player(&home);

    plan_spike(&repo, &mut player, "probe");
    director_commit_on_branch(
        repo.path(),
        "task/after",
        "tasks/after/task.toml",
        "title = \"after\"\nphase = \"p1\"\ndepends_on = [\"probe\"]\n",
        "plan: after",
    );
    let report = player.tick().unwrap();
    let msg = one_rejection(&report, Reason::Plan);
    assert!(msg.contains("nothing may depend on a spike"), "{msg}");
}

// --------------------------------------------------------------------------
// Post and launch
// --------------------------------------------------------------------------

#[test]
fn post_launches_with_a_base_pin_and_a_spike_prompt_and_no_suite_pins() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);

    plan_and_post(&repo, &mut player, "probe");

    let tip = hub.rev_parse("refs/heads/task/probe").unwrap();
    assert_eq!(
        records::base_commit(&hub, "probe"),
        Some(tip),
        "the base is the post tip, set at launch"
    );
    assert!(records::has_pins(&hub, "probe"));
    let pinned = records::pinned_files(&hub, "probe").unwrap();
    assert!(
        pinned.contains_key("tasks/probe/questions/1.toml"),
        "{pinned:?}"
    );
    assert!(
        pinned.contains_key("tasks/probe/baseline/1.toml"),
        "{pinned:?}"
    );
    assert!(
        !pinned.contains_key("tests.toml"),
        "a spike doesn't pin the project suite: {pinned:?}"
    );

    let call = &player.sessions().launched[0];
    let prompt = std::fs::read_to_string(&call.prompt_file).unwrap();
    assert!(prompt.contains("SPIKE `probe`"), "{prompt}");
    assert!(prompt.contains("find out"), "{prompt}");
    assert!(
        prompt.contains("## The question (round 1)")
            && prompt.contains("Can we produce feature.txt?"),
        "{prompt}"
    );
}

#[test]
fn post_is_rejected_when_a_baseline_claim_fails_on_the_current_code() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let mut player = open_player(&home);

    plan_spike(&repo, &mut player, "probe");
    let report = post_spike_with(
        &repo,
        &mut player,
        "probe",
        &[(
            "tasks/probe/baseline/1.toml",
            "[[claim]]\nname = \"wrong\"\ncmd = \"test -f feature.txt\"\n",
        )],
    );
    let msg = one_rejection(&report, Reason::Post);
    assert!(
        msg.contains("baseline claims must pass on the current code") && msg.contains("wrong"),
        "{msg}"
    );
    assert!(report.launched.is_empty());
}

#[test]
fn post_is_rejected_without_a_question_or_with_stray_paths() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let mut player = open_player(&home);

    plan_spike(&repo, &mut player, "probe");
    checkout_branch(repo.path(), "task/probe");
    write_file(repo.path(), "tasks/probe/prompt.md", "find out\n");
    commit(repo.path(), "post: no question");
    let msg = one_rejection(&player.tick().unwrap(), Reason::Post);
    assert!(
        msg.contains("must add exactly tasks/probe/questions/1.toml"),
        "{msg}"
    );

    // Fix the question but smuggle in a code change.
    write_file(repo.path(), "tasks/probe/questions/1.toml", QUESTION);
    write_file(repo.path(), "src/sneaky.rs", "fn main() {}\n");
    commit(repo.path(), "post: with code");
    let msg = one_rejection(&player.tick().unwrap(), Reason::Post);
    assert!(msg.contains("may only add the director's files"), "{msg}");
}

#[test]
fn post_may_not_pre_write_the_workers_findings_or_claims() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let mut player = open_player(&home);

    plan_spike(&repo, &mut player, "probe");
    let report = post_spike_with(
        &repo,
        &mut player,
        "probe",
        &[("tasks/probe/findings/1.md", "already?\n")],
    );
    let msg = one_rejection(&report, Reason::Post);
    assert!(
        msg.contains("may only add the director's files") && msg.contains("findings/1.md"),
        "{msg}"
    );
}

#[test]
fn post_is_rejected_for_an_invalid_question() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let mut player = open_player(&home);

    plan_spike(&repo, &mut player, "probe");
    checkout_branch(repo.path(), "task/probe");
    write_file(repo.path(), "tasks/probe/prompt.md", "find out\n");
    write_file(
        repo.path(),
        "tasks/probe/questions/1.toml",
        "question = \"Can we?\"\n",
    );
    commit(repo.path(), "post");
    let msg = one_rejection(&player.tick().unwrap(), Reason::Post);
    assert!(
        msg.contains("answers must list at least one answer"),
        "{msg}"
    );
}

// --------------------------------------------------------------------------
// Finish
// --------------------------------------------------------------------------

#[test]
fn a_valid_finish_is_tested_and_the_player_records_the_verified_answer() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");

    let mut files = good_finish_files("probe");
    // The suite is not required: the worker may break the project's tests.
    files.push((
        "tests.toml".to_string(),
        "[[check]]\nname = \"unit\"\ncmd = \"false\"\n".to_string(),
    ));
    // Claims may report measurements.
    files.push((
        "tasks/probe/claims/measure.sh".to_string(),
        "test -f feature.txt || exit 1\necho 'METRICS {\"throughput\": 2.1}'\n".to_string(),
    ));
    files[2].1 = claims_toml(
        "yes",
        "[[claim]]\nname = \"has-feature\"\nkind = \"spike\"\nsupports = \"yes\"\ncmd = \"sh tasks/probe/claims/measure.sh\"\n",
    );
    let report = finish_with(&repo, &mut player, "probe", &files);

    assert!(report.rejected.is_empty(), "{report}");
    assert!(
        report
            .imported
            .iter()
            .any(|(_, _, t)| *t == Transition::Finish),
        "{report}"
    );
    assert_eq!(status_of(&hub, "probe"), Status::Tested);

    let tip = hub.rev_parse("refs/heads/task/probe").unwrap();
    let note = records::exploration(&hub, &tip).expect("exploration note on the finish commit");
    assert_eq!(
        (note.round, note.answer.as_str(), note.concluded),
        (1, "yes", false)
    );
    assert_eq!(note.question, "Can we produce feature.txt?");
    let claim = &note.claims["has-feature"];
    assert!(claim.passed);
    assert_eq!(claim.metrics, Some(serde_json::json!({"throughput": 2.1})));
    assert!(
        note.claims.contains_key("no-feature-yet"),
        "the director's baseline claim is recorded too: {:?}",
        note.claims.keys()
    );

    let sent = &player.sessions().sent;
    assert!(
        sent.iter()
            .any(|(_, m)| m.contains("claims verified (answer: yes)")),
        "{sent:?}"
    );
}

#[test]
fn an_inconclusive_answer_needs_no_claims() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");

    let files = vec![
        (
            "tasks/probe/findings/1.md".to_string(),
            "couldn't tell\n".to_string(),
        ),
        (
            "tasks/probe/claims/1.toml".to_string(),
            "answer = \"inconclusive\"\n".to_string(),
        ),
    ];
    let report = finish_with(&repo, &mut player, "probe", &files);
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(status_of(&hub, "probe"), Status::Tested);
}

#[test]
fn finish_is_rejected_without_findings() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");

    let mut files = good_finish_files("probe");
    files.remove(1);
    let msg = one_rejection(
        &finish_with(&repo, &mut player, "probe", &files),
        Reason::Claims,
    );
    assert!(msg.contains("findings/1.md is missing or empty"), "{msg}");
    assert_eq!(strikes(&hub, "probe", Reason::Claims), 1);

    // An empty findings file doesn't count either.
    let files = vec![("tasks/probe/findings/1.md".to_string(), "  \n".to_string())];
    let msg = one_rejection(
        &finish_with(&repo, &mut player, "probe", &files),
        Reason::Claims,
    );
    assert!(msg.contains("findings/1.md is missing or empty"), "{msg}");
}

#[test]
fn finish_is_rejected_when_the_answer_is_not_allowed_or_unsupported() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");

    // An answer the question doesn't offer.
    let mut files = good_finish_files("probe");
    files[2].1 = claims_toml("maybe", &spike_claim("has-feature", "yes"));
    let msg = one_rejection(
        &finish_with(&repo, &mut player, "probe", &files),
        Reason::Claims,
    );
    assert!(
        msg.contains("answer \"maybe\" is not one of the question's answers"),
        "{msg}"
    );

    // A real answer with no claim backing it.
    let mut files = good_finish_files("probe");
    files[2].1 = claims_toml("no", &spike_claim("has-feature", "yes"));
    let report = finish_with(&repo, &mut player, "probe", &files);
    let msg = one_rejection(&report, Reason::Claims);
    assert!(
        msg.contains("answer \"no\" needs at least one spike claim with supports = \"no\""),
        "{msg}"
    );
}

#[test]
fn finish_is_rejected_when_a_claim_file_is_missing_or_malformed() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");

    let files = vec![
        ("feature.txt".to_string(), "x\n".to_string()),
        (
            "tasks/probe/findings/1.md".to_string(),
            "done\n".to_string(),
        ),
    ];
    let msg = one_rejection(
        &finish_with(&repo, &mut player, "probe", &files),
        Reason::Claims,
    );
    assert!(msg.contains("claims/1.toml is missing"), "{msg}");

    let files = vec![(
        "tasks/probe/claims/1.toml".to_string(),
        "answer = \"yes\"\n[[claim]]\nname = \"c\"\ncmd = \"\"\n".to_string(),
    )];
    let msg = one_rejection(
        &finish_with(&repo, &mut player, "probe", &files),
        Reason::Claims,
    );
    assert!(msg.contains("cmd must not be empty"), "{msg}");
}

#[test]
fn a_spike_claim_that_also_passes_without_the_work_is_rejected() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");

    let mut files = good_finish_files("probe");
    files[2].1 = claims_toml(
        "yes",
        "[[claim]]\nname = \"trivial\"\nkind = \"spike\"\nsupports = \"yes\"\ncmd = \"true\"\n",
    );
    let msg = one_rejection(
        &finish_with(&repo, &mut player, "probe", &files),
        Reason::Claims,
    );
    assert!(
        msg.contains("\"trivial\"") && msg.contains("also passes on the base commit"),
        "{msg}"
    );
}

#[test]
fn a_spike_claim_that_fails_on_the_branch_is_rejected_with_its_output() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");

    let mut files = good_finish_files("probe");
    files[2].1 = claims_toml(
        "yes",
        "[[claim]]\nname = \"broken\"\nkind = \"spike\"\nsupports = \"yes\"\ncmd = \"echo no luck >&2; exit 3\"\n",
    );
    let msg = one_rejection(
        &finish_with(&repo, &mut player, "probe", &files),
        Reason::Claims,
    );
    assert!(
        msg.contains("\"broken\"")
            && msg.contains("fails on your branch")
            && msg.contains("no luck"),
        "{msg}"
    );
}

#[test]
fn one_failing_claim_rejects_the_whole_finish_even_if_the_supporting_claim_passes() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");

    let mut files = good_finish_files("probe");
    let extra = "[[claim]]\nname = \"side-claim\"\nkind = \"spike\"\ncmd = \"false\"\n";
    files[2].1 = claims_toml(
        "yes",
        &format!("{}\n{extra}", spike_claim("has-feature", "yes")),
    );
    let msg = one_rejection(
        &finish_with(&repo, &mut player, "probe", &files),
        Reason::Claims,
    );
    assert!(msg.contains("\"side-claim\""), "{msg}");
    assert!(
        !msg.contains("\"has-feature\""),
        "the passing claim isn't blamed: {msg}"
    );
}

#[test]
fn a_baseline_claim_that_fails_on_the_base_is_rejected() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");

    let mut files = good_finish_files("probe");
    let bad = "[[claim]]\nname = \"wrong-baseline\"\nkind = \"baseline\"\ncmd = \"test -f feature.txt\"\n";
    files[2].1 = claims_toml(
        "yes",
        &format!("{}\n{bad}", spike_claim("has-feature", "yes")),
    );
    let msg = one_rejection(
        &finish_with(&repo, &mut player, "probe", &files),
        Reason::Claims,
    );
    assert!(
        msg.contains("\"wrong-baseline\"")
            && msg.contains("baseline claim and fails on the base commit"),
        "{msg}"
    );
}

#[test]
fn a_claim_that_outruns_its_timeout_fails() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");

    let mut files = good_finish_files("probe");
    files[2].1 = claims_toml(
        "yes",
        "[[claim]]\nname = \"slow\"\nkind = \"spike\"\nsupports = \"yes\"\ntimeout = 1\ncmd = \"sleep 20\"\n",
    );
    let msg = one_rejection(
        &finish_with(&repo, &mut player, "probe", &files),
        Reason::Claims,
    );
    assert!(
        msg.contains("\"slow\"") && msg.contains("timed out after 1s"),
        "{msg}"
    );
}

#[test]
fn finish_may_not_touch_the_directors_files_or_other_rounds() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");

    // Editing a pinned file is a pins strike, as for any task.
    let mut files = good_finish_files("probe");
    files.push((
        "tasks/probe/questions/1.toml".to_string(),
        "question = \"easier?\"\nanswers = [\"yes\"]\n".to_string(),
    ));
    let msg = one_rejection(
        &finish_with(&repo, &mut player, "probe", &files),
        Reason::Pins,
    );
    assert!(msg.contains("questions/1.toml"), "{msg}");

    // So is smuggling in the next round's question, which isn't pinned yet.
    let dir = paths::worker_clone(&repo.director_path(), "probe");
    Git::new(&dir)
        .run(&[
            "reset",
            "-q",
            "--hard",
            &hub.rev_parse("refs/heads/task/probe").unwrap(),
        ])
        .unwrap();
    let mut files = good_finish_files("probe");
    files.push((
        "tasks/probe/questions/2.toml".to_string(),
        QUESTION.to_string(),
    ));
    let report = finish_with(&repo, &mut player, "probe", &files);
    let msg = report
        .rejected
        .last()
        .map(|r| r.message.clone())
        .unwrap_or_default();
    assert!(
        msg.contains("questions/2.toml") && msg.contains("aren't the worker's"),
        "{report}"
    );
}

#[test]
fn claims_strikes_escalate_at_their_own_limit() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");

    let bad = vec![("tasks/probe/findings/1.md".to_string(), "x\n".to_string())];
    finish_with(&repo, &mut player, "probe", &bad);
    assert_ne!(status_of(&hub, "probe"), Status::Escalated);
    let bad = vec![("tasks/probe/findings/1.md".to_string(), "y\n".to_string())];
    finish_with(&repo, &mut player, "probe", &bad);
    assert_eq!(strikes(&hub, "probe", Reason::Claims), 2);
    assert_eq!(status_of(&hub, "probe"), Status::Escalated);
}

#[test]
fn a_block_escalates_a_spike_like_any_task() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");

    let dir = paths::worker_clone(&repo.director_path(), "probe");
    Git::new(&dir).run(&["config", "user.name", "S"]).unwrap();
    Git::new(&dir)
        .run(&["config", "user.email", "s@example.com"])
        .unwrap();
    commit_empty(&dir, "block: no way to tell");
    player.tick().unwrap();
    assert_eq!(status_of(&hub, "probe"), Status::Escalated);
}

// --------------------------------------------------------------------------
// Send-back
// --------------------------------------------------------------------------

fn finish_round_one(repo: &DirectorRepo, player: &mut Player<NullSessions>) {
    let report = finish_with(repo, player, "probe", &good_finish_files("probe"));
    assert!(report.rejected.is_empty(), "{report}");
}

/// The director's send-back: the hub's tip (the finish) fetched and checked
/// out, then `extra` files committed.
fn send_back(
    repo: &DirectorRepo,
    player: &mut Player<NullSessions>,
    files: &[(&str, &str)],
) -> TickReport {
    director_pull_task_branch(repo.path(), "probe");
    for (rel, content) in files {
        write_file(repo.path(), rel, content);
    }
    commit(repo.path(), "send back");
    player.tick().expect("send-back tick")
}

const QUESTION_2: &str = "question = \"Can we produce feature.txt twice as fast?\"\nanswers = [\"yes\", \"no\"]\nrefines = 1\n";

#[test]
fn send_back_adds_the_next_question_and_relays_it() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");
    finish_round_one(&repo, &mut player);

    let report = send_back(
        &repo,
        &mut player,
        &[("tasks/probe/questions/2.toml", QUESTION_2)],
    );
    assert!(report.rejected.is_empty(), "{report}");
    assert!(
        report
            .imported
            .iter()
            .any(|(_, _, t)| *t == Transition::SendBack),
        "{report}"
    );
    assert_eq!(status_of(&hub, "probe"), Status::InRevision);

    let hub_state = state::derive(&hub, &|_| false).unwrap();
    let t = hub_state.tasks.iter().find(|t| t.slug == "probe").unwrap();
    assert_eq!(t.spike.as_ref().unwrap().round, 2);
    assert_eq!(t.review_round, 1);

    let pinned = records::pinned_files(&hub, "probe").unwrap();
    assert!(
        pinned.contains_key("tasks/probe/questions/2.toml"),
        "re-pinned: {pinned:?}"
    );
    assert!(
        pinned.contains_key("tasks/probe/claims/1.toml"),
        "round 1's evidence is pinned now: {pinned:?}"
    );

    let (_, msg) = player.sessions().sent.last().unwrap();
    assert!(
        msg.contains("Round 2") && msg.contains("twice as fast") && msg.contains("findings/2.md"),
        "{msg}"
    );
}

#[test]
fn send_back_needs_no_failing_test_but_only_adds_question_files() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");
    finish_round_one(&repo, &mut player);

    // Adding code is not a send-back.
    let report = send_back(
        &repo,
        &mut player,
        &[
            ("tasks/probe/questions/2.toml", QUESTION_2),
            ("src/x.rs", "fn x() {}\n"),
        ],
    );
    let msg = one_rejection(&report, Reason::Review);
    assert!(msg.contains("may only add"), "{msg}");
}

#[test]
fn send_back_is_rejected_without_a_question_with_the_wrong_round_or_a_bad_refines() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");
    finish_round_one(&repo, &mut player);

    // A note is not a question.
    let report = send_back(
        &repo,
        &mut player,
        &[("tasks/probe/reviews/r1.md", "try harder\n")],
    );
    let msg = one_rejection(&report, Reason::Ownership);
    assert!(msg.contains("doesn't add a question file"), "{msg}");

    // Round 3 before round 2.
    let report = send_back(
        &repo,
        &mut player,
        &[("tasks/probe/questions/3.toml", QUESTION_2)],
    );
    let last = report.rejected.last().unwrap();
    assert_eq!(last.reason, Reason::Review);
    assert!(
        last.message.contains("expected round 2"),
        "{}",
        last.message
    );
}

#[test]
fn send_back_rejects_an_out_of_range_refines_and_failing_baseline_claims() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");
    finish_round_one(&repo, &mut player);

    let bad = "question = \"q\"\nanswers = [\"a\"]\nrefines = 7\n";
    let msg = one_rejection(
        &send_back(&repo, &mut player, &[("tasks/probe/questions/2.toml", bad)]),
        Reason::Review,
    );
    assert!(msg.contains("refines = 7"), "{msg}");

    // The director's new baseline claim must hold on the *base* code, not on
    // the worker's branch (where feature.txt now exists).
    let report = send_back(
        &repo,
        &mut player,
        &[
            ("tasks/probe/questions/2.toml", QUESTION_2),
            (
                "tasks/probe/baseline/2.toml",
                "[[claim]]\nname = \"base-has-feature\"\ncmd = \"test -f feature.txt\"\n",
            ),
        ],
    );
    let last = report.rejected.last().unwrap();
    assert!(
        last.message
            .contains("baseline claims must pass on the current code")
            && last.message.contains("base-has-feature"),
        "{}",
        last.message
    );
}

#[test]
fn a_new_baseline_claim_is_checked_against_the_base_not_the_branch() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");
    finish_round_one(&repo, &mut player);

    // Holds on the base (no feature.txt there) though not on the branch.
    let report = send_back(
        &repo,
        &mut player,
        &[
            ("tasks/probe/questions/2.toml", QUESTION_2),
            (
                "tasks/probe/baseline/2.toml",
                "[[claim]]\nname = \"still-none\"\ncmd = \"test ! -f feature.txt\"\n",
            ),
        ],
    );
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(status_of(&hub, "probe"), Status::InRevision);
}

#[test]
fn round_two_finish_is_judged_on_round_two_files_and_round_one_stays_pinned() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");
    finish_round_one(&repo, &mut player);
    send_back(
        &repo,
        &mut player,
        &[("tasks/probe/questions/2.toml", QUESTION_2)],
    );
    pull_in_clone(&repo, "probe");

    // Round 1's findings alone don't answer round 2.
    let msg = one_rejection(
        &finish_with(
            &repo,
            &mut player,
            "probe",
            &[("src/more.rs".to_string(), "fn more() {}\n".to_string())],
        ),
        Reason::Claims,
    );
    assert!(msg.contains("findings/2.md is missing or empty"), "{msg}");

    // Rewriting round 1's claims is a pins violation.
    let files = vec![(
        "tasks/probe/claims/1.toml".to_string(),
        "answer = \"inconclusive\"\n".to_string(),
    )];
    let msg = one_rejection(
        &finish_with(&repo, &mut player, "probe", &files),
        Reason::Pins,
    );
    assert!(msg.contains("claims/1.toml"), "{msg}");

    // Reset the clone to the hub so a clean round-2 commit can follow the
    // rejected ones, then answer round 2.
    let dir = paths::worker_clone(&repo.director_path(), "probe");
    Git::new(&dir)
        .run(&[
            "reset",
            "-q",
            "--hard",
            &hub.rev_parse("refs/heads/task/probe").unwrap(),
        ])
        .unwrap();
    let files = vec![
        ("tasks/probe/findings/2.md".to_string(), "faster: no\n".to_string()),
        ("tasks/probe/claims/2.toml".to_string(), "answer = \"no\"\n\n[[claim]]\nname = \"still-built\"\nkind = \"spike\"\nsupports = \"no\"\ncmd = \"test -f feature.txt\"\n".to_string()),
    ];
    let report = finish_with(&repo, &mut player, "probe", &files);
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(status_of(&hub, "probe"), Status::Tested);

    let hub_state = state::derive(&hub, &|_| false).unwrap();
    let sp = hub_state
        .tasks
        .iter()
        .find(|t| t.slug == "probe")
        .unwrap()
        .spike
        .clone()
        .unwrap();
    assert_eq!(
        (sp.round, sp.answer.as_deref(), sp.concluded),
        (2, Some("no"), false)
    );
}

#[test]
fn too_many_send_backs_escalate_instead_of_importing() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");
    finish_round_one(&repo, &mut player);

    // max_review_rounds defaults to 2: send-backs 1 and 2 are fine ...
    for round in 2..=3u32 {
        let q = format!(
            "question = \"round {round}?\"\nanswers = [\"yes\", \"no\"]\nrefines = {}\n",
            round - 1
        );
        let report = send_back(
            &repo,
            &mut player,
            &[(&format!("tasks/probe/questions/{round}.toml"), &q)],
        );
        assert!(report.rejected.is_empty(), "{report}");
        pull_in_clone(&repo, "probe");
        let files = vec![
            (
                format!("tasks/probe/findings/{round}.md"),
                "ok\n".to_string(),
            ),
            (
                format!("tasks/probe/claims/{round}.toml"),
                claims_toml("yes", &spike_claim(&format!("c{round}"), "yes")),
            ),
        ];
        let report = finish_with(&repo, &mut player, "probe", &files);
        assert!(report.rejected.is_empty(), "{report}");
    }
    // ... and the third escalates.
    let q = "question = \"round 4?\"\nanswers = [\"yes\"]\nrefines = 3\n";
    let report = send_back(&repo, &mut player, &[("tasks/probe/questions/4.toml", q)]);
    assert_eq!(report.escalated.len(), 1, "{report}");
    assert!(
        report.escalated[0].1.contains("review rounds exceeded"),
        "{report}"
    );
    assert_eq!(status_of(&hub, "probe"), Status::Escalated);
}

// --------------------------------------------------------------------------
// Conclude, accept, retire
// --------------------------------------------------------------------------

fn conclude(repo: &DirectorRepo, player: &mut Player<NullSessions>) -> TickReport {
    director_pull_task_branch(repo.path(), "probe");
    commit_empty(repo.path(), "conclude: yes, we can\n\nOperator agreed.");
    player.tick().expect("conclude tick")
}

#[test]
fn conclude_moves_the_branch_to_explored_and_records_the_answer() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");
    finish_round_one(&repo, &mut player);

    let main_before = hub.rev_parse("refs/heads/main").unwrap();
    let report = conclude(&repo, &mut player);
    assert!(report.rejected.is_empty(), "{report}");
    assert!(
        report
            .imported
            .iter()
            .any(|(r, _, t)| r == "refs/explored/probe" && *t == Transition::Conclude),
        "{report}"
    );

    assert!(records::is_explored(&hub, "probe"));
    assert!(
        !hub.ref_exists("refs/heads/task/probe"),
        "the live branch moves, like a retirement"
    );
    assert_eq!(
        hub.rev_parse("refs/heads/main").unwrap(),
        main_before,
        "a spike never moves main"
    );
    assert_eq!(player.sessions().killed, vec!["probe".to_string()]);
    assert!(
        !paths::worker_clone(&repo.director_path(), "probe").exists(),
        "its worktree is reclaimed"
    );

    assert_eq!(status_of(&hub, "probe"), Status::Explored);
    let hub_state = state::derive(&hub, &|_| false).unwrap();
    let sp = hub_state
        .tasks
        .iter()
        .find(|t| t.slug == "probe")
        .unwrap()
        .spike
        .clone()
        .unwrap();
    assert_eq!(
        (sp.round, sp.answer.as_deref(), sp.concluded),
        (1, Some("yes"), true)
    );

    // Concluding emitted an event for the director.
    let events = dashboard::events::read_all(&home);
    assert!(
        events
            .iter()
            .any(|e| e.kind == dashboard::events::EventKind::Explored && e.message.contains("yes")),
        "{events:?}"
    );

    // The slug is spent, like a retired one, and a stale local branch is ignored.
    let report = player.tick().unwrap();
    assert!(report.is_empty(), "{report}");
}

#[test]
fn conclude_is_rejected_unless_the_spike_is_tested() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");

    // Posted, nothing validated yet.
    checkout_branch(repo.path(), "task/probe");
    commit_empty(repo.path(), "conclude: early");
    let msg = one_rejection(&player.tick().unwrap(), Reason::Accept);
    assert!(msg.contains("needs a validated finish"), "{msg}");
    assert!(!records::is_explored(&hub, "probe"));
}

#[test]
fn conclude_on_an_ordinary_task_is_ownership() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let mut player = open_player(&home);

    director_commit_on_branch(
        repo.path(),
        "task/plain",
        "tasks/plain/task.toml",
        "title = \"plain\"\nphase = \"p1\"\n",
        "plan: plain",
    );
    player.tick().unwrap();
    checkout_branch(repo.path(), "task/plain");
    commit_empty(repo.path(), "conclude: nope");
    let msg = one_rejection(&player.tick().unwrap(), Reason::Ownership);
    assert!(msg.contains("not a spike"), "{msg}");
}

#[test]
fn a_spike_never_merges_so_accept_is_rejected() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");
    finish_round_one(&repo, &mut player);

    director_pull_task_branch(repo.path(), "probe");
    commit_empty(repo.path(), "accept: please");
    let msg = one_rejection(&player.tick().unwrap(), Reason::Accept);
    assert!(
        msg.contains("never merges") && msg.contains("conclude:"),
        "{msg}"
    );
    assert_eq!(status_of(&hub, "probe"), Status::Tested);
}

#[test]
fn a_tested_spike_can_be_retired_when_the_operator_gives_up() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");
    finish_round_one(&repo, &mut player);

    director_pull_task_branch(repo.path(), "probe");
    commit_empty(
        repo.path(),
        "retire: not worth pursuing\n\nThe answer doesn't change the plan.",
    );
    let report = player.tick().unwrap();
    assert!(report.rejected.is_empty(), "{report}");
    assert_eq!(status_of(&hub, "probe"), Status::Retired);
}

#[test]
fn an_explored_slug_is_never_reused() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");
    finish_round_one(&repo, &mut player);
    conclude(&repo, &mut player);

    director_commit_on_branch(
        repo.path(),
        "task/second",
        "tasks/second/task.toml",
        "title = \"second\"\nphase = \"p1\"\ndepends_on = [\"probe\"]\n",
        "plan: second",
    );
    let msg = one_rejection(&player.tick().unwrap(), Reason::Plan);
    assert!(msg.contains("depends on spike"), "{msg}");
}

// --------------------------------------------------------------------------
// One at a time, and status
// --------------------------------------------------------------------------

#[test]
fn a_spike_runs_beside_another_spike() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");

    plan_spike(&repo, &mut player, "second");
    let report = post_spike_with(&repo, &mut player, "second", &[]);
    assert!(
        report.rejected.is_empty(),
        "spikes run in parallel too (spec §4): {report}"
    );
    assert_eq!(status_of(&hub, "second"), Status::Posted);
}

#[test]
fn status_lists_spikes_apart_from_the_progress_counts() {
    let repo = director_repo();
    let (_h, home) = init_hub(&repo);
    let hub = Git::new(home.hub());
    let mut player = open_player(&home);
    plan_and_post(&repo, &mut player, "probe");
    finish_round_one(&repo, &mut player);

    let hub_state = state::derive(&hub, &|_| false).unwrap();
    let out = render::render(&hub_state, &[], false, 100);
    assert!(out.contains("◇ Spikes (1)"), "{out}");
    assert!(out.contains("round 1, latest answer: yes"), "{out}");
    assert!(
        out.contains("0/0 accepted"),
        "a spike isn't part of the progress denominator: {out}"
    );
    assert!(
        out.contains("claims: "),
        "the in-flight block shows the spike's claims: {out}"
    );

    conclude(&repo, &mut player);
    let hub_state = state::derive(&hub, &|_| false).unwrap();
    let out = render::render(&hub_state, &[], false, 100);
    assert!(
        out.contains("explored") && out.contains("answer: yes (round 1)"),
        "{out}"
    );
}
