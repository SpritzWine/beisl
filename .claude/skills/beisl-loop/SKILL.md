---
name: beisl-loop
description: One iteration of the beisl development loop — review plan, pick next backlog item, implement it on a feature branch via subagents, verify, strategic-fit review, land on the agent-dev integration branch, queue for human batch review. Run continuously via /loop, or once standalone. `review` arg produces the human digest instead.
---

# beisl agentic loop

One invocation = **one iteration** = one backlog item taken from plan to a
review-ready branch. With arg `review`, skip the iteration and produce the
digest (step 9).

Branch model: per-item branches `loop/<slug>` are cut from **`agent-dev`**
(the integration branch, itself based on main) and merged back into it with
`--no-ff` — so stacked items see each other's work, every item stays one
revertible merge commit, and **main is never touched by the loop**. Florian
merges `agent-dev` → main in batch after review.

State lives in `.loop/` (gitignored, main worktree — the loop's own scratch,
human-editable):

- `.loop/BACKLOG.md` — prioritized items. `[needs-human]` items are never picked.
- `.loop/REVIEW-QUEUE.md` — landed items awaiting Florian's review.

## Guardrails (every iteration, every subagent)

- **Headless, always.** Never open a window on Florian's desktop: every beisl
  invocation gets `BEISL_NO_UI=1`; never execute `beisl-setup` or any GUI
  binary. GUI-touching items are verified by `cargo check`/`clippy`/unit
  tests only and their queue entry is flagged **`[visual check needed]`** —
  the human looks at the window, not the loop.
- **No privilege, no real state.** No `pkexec`/`sudo`, ever (preflight fixes
  are *described*, per crate policy). Never run `beisl install`/`uninstall`
  or anything touching the real `$HOME` — use the verify skill's isolated
  `XDG_*`/scratch-HOME pattern. No pushes to any remote.
- **Git hygiene.** Direct commits only on `loop/*` branches; `agent-dev`
  receives merges only; main receives only human-approved landings (step 9).
  No history rewriting outside an unmerged `loop/*` branch.
- **The main checkout is Florian's.** Never `git checkout`, `reset`, or
  `merge` in it — he may be mid-work there (e.g. the MCP crate) and the loop
  must not switch branches under him (this bit us on 2026-07-11: a landing
  left his tree on agent-dev). Every ref-touching operation happens in
  worktrees: per-item worktrees under `.loop/wt/<slug>`, and the **standing
  `.loop/wt/agent-dev` worktree** (create with
  `git worktree add .loop/wt/agent-dev agent-dev` if missing) for step 0's
  sync and step 7's `--no-ff` merges. Exception: step 9's landing on main is
  human-gated and interactive — coordinate in chat if his checkout is dirty.
- **Build economy.** Worktrees share one target dir so iterations don't
  cold-build: export `CARGO_TARGET_DIR=<main-repo>/.loop/target` in every
  worktree shell. Build `crates/beisl-gui` only when the item touches it
  (separate workspace, heavy git deps — its first build is the slow one).
- **Model/effort economy.** Match the tier to the chunk, never above the
  session model:
  - mechanical edits, docs, log-formatting → `haiku` / low effort
  - standard implementation + tests → `sonnet` / default effort
  - decomposition planning, strategic-fit review → session model, high effort
  - ≤ 4 concurrent subagents unless a Workflow's verified fan-out justifies
    more. No WebSearch/WebFetch unless the item genuinely needs external
    docs (e.g. MangoHud config semantics) — this repo + DESIGN.md answer
    most questions.

## Iteration protocol

### 0. Preconditions & sync

- Read `.loop/BACKLOG.md`, `.loop/REVIEW-QUEUE.md`, the DESIGN.md Roadmap
  section, and CLAUDE.md "Known shortcomings".
- Review does NOT throttle the loop: it keeps iterating with any number of
  unreviewed queue entries (Florian's call, 2026-07-11). Human review gates
  only the landing on main (step 9).
- **Sync `agent-dev`** — inside the standing `.loop/wt/agent-dev` worktree,
  never in the main checkout: create the branch at main HEAD if missing. If
  the queue has no unreviewed entries (all merged/discarded by Florian),
  reset it to main. Otherwise, if main gained commits, merge main into
  agent-dev; a conflict here means human work and loop work collided — stop
  and tell Florian instead of resolving on his behalf.
- Uncommitted changes in the main checkout are invisible to worktrees
  branched from HEAD — if they overlap the item about to be picked, pick a
  non-overlapping one; if everything overlaps, stop and say main needs a
  commit first.

### 1. Review the plan

Reconcile the backlog against reality: drop items already done (check git
log — including agent-dev — and the code, not just the queue), add newly
discovered follow-ups (e.g. from strategic-fit notes), re-order if DESIGN.md
changed. Backlog is a cache of the plan; DESIGN.md is the source of truth.

### 2. Pick the next item

Topmost item that is: not `[needs-human]`, not `[blocked]`, not already in
the review queue, not overlapping uncommitted work on main, and sized for one
iteration. Too big → split it, write the sub-items back to the backlog, pick
the first.

### 3. Plan the distribution

Decide the decomposition before touching code — a Plan agent for anything
non-trivial, inline for small items. Independent chunks → parallel subagents
(tiers per Guardrails); wide fan-out with verification stages → the Workflow
tool (this skill is the explicit opt-in); small single-file items → inline.
Don't orchestrate for show.

### 4. Feature branch, isolated

Branch `loop/<item-slug>` **from `agent-dev`**. Implement in a worktree
(EnterWorktree, or `isolation: 'worktree'` on the agents) — never in the
main checkout, so Florian's working tree is untouched while the loop runs.

### 5. Implement + test feedback loop

Respect CLAUDE.md conventions (std-only errors, no log crate, inline tests,
crates.io-only deps in the main workspace). Verify until green:

- `cargo fmt && cargo check --workspace && cargo clippy --workspace && cargo test --workspace`
- Wrapper behavior changed → drive it end-to-end with the **verify** skill
  (headless fake game, isolated XDG dirs, `BEISL_NO_UI=1`).
- `crates/beisl-gui` touched → also `(cd crates/beisl-gui && cargo check)`;
  no visual run (see Guardrails), flag the queue entry instead.

Three failed fix attempts on the same failure → stop fighting: mark the item
`[blocked]` in the backlog with what was learned, clean up the branch, and
end the iteration (a valid outcome — record it, don't hide it).

### 6. Strategic-fit review

A **fresh-context reviewer agent** (session model, high effort) gets the diff
plus DESIGN.md and CLAUDE.md, and answers: does this serve the project goals
(wine-agnostic, light main workspace, conventions kept, roadmap direction) —
or is it locally clever but strategically wrong? Verdict `fits` / `revise`
(one revision round, then re-review) / `misfit` (drop the branch, note why in
the backlog — a misfit is cheaper abandoned than merged).

### 7. Land on `agent-dev`

Commit on the branch (conventional message, `Co-Authored-By: Claude Fable 5
<noreply@anthropic.com>`), then `git merge --no-ff loop/<slug>` into
`agent-dev` — run inside the `.loop/wt/agent-dev` worktree, not the main
checkout — and record the merge-commit hash: that hash is the unit of
review and of revert.

### 8. Queue for human review

Append to `.loop/REVIEW-QUEUE.md`:

```markdown
## <date> `loop/<slug>` — <item title>          [unreviewed]
- What: <2-3 sentences, plain language>
- Verified: <exact commands run + results; verify-skill evidence if used>
- Strategic fit: <verdict + one-line reasoning from the reviewer>
- Depends on: <earlier queued branch, or "—">
- Flags: <[visual check needed] if GUI-touching, else "—">
- Merge commit: <hash> — inspect: `git show <hash>`
```

### 9. Loop control, digest, landing on main

Running under /loop: ScheduleWakeup (~900–1800 s pacing; the same /loop
prompt) unless a stop condition hit — backlog empty or two consecutive
blocked iterations. **On every stop, and on the `review` arg:** post the
"what lately was done" digest as the final message — per queued item: title,
what/why, diffstat, verification evidence, fit verdict, flags/dependencies;
plus anything `[blocked]` and what the loop learned.

**Landing on main (the human gate).** Only Florian's approval — in chat or
by marking queue entries — moves work to main. Whatever the shape, messages
read like regular development: NO trace-run IDs, loop/reviewer/
revision-round mechanics, debugging narrative, or `.loop/` references —
motivation phrased in terms of the problem and design, per the project's
why-style. Two shapes, by item size:

- **Tiny, or squashes to one atomic commit** (the common case): land it as a
  single plain commit directly on main — no `land/*` branch, no merge
  commit; the ceremony would outweigh the change. Small mechanical fallout
  (fmt of neighboring files) folds in with a message note rather than
  earning a second commit.
- **Genuinely multi-commit items** (real reviewable seams — e.g. a
  wide mechanical rename apart from the behavior change): compose a
  `land/<slug>` branch from main with those clean commits, then
  `git merge --no-ff land/<slug>` with a normal feature-headline merge
  message, so main's first-parent history stays a sequence of feature-sized
  steps while the detail sits one level down.

The forensic detail stays on the `loop/*` branches and in the queue entries.
A rejected item is instead `git revert -m 1 <merge-commit>`-ed on agent-dev
and its branch dropped, with the reason noted in the backlog.

After landing: reset `agent-dev` onto main (in its worktree), delete the
landed `land/*` and `loop/*` branches, and remove (or mark `[reviewed]`)
the queue entries.
