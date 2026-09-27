# dashboard

`dashboard` is the enforcement point for a subagent-per-task roadmap loop.
It keeps a bare git hub as the single source of truth and runs a *player*
process that imports and validates every commit a director or subagent
produces, runs every check itself, launches subagents in `tmux`, counts
strikes, merges accepted work, and retires dead ends. The status it renders
is a pure function of the hub: nobody writes a status by hand. See
[`spec.md`](spec.md) for the full design.
