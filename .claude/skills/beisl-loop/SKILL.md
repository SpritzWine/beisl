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
  receives merges only; main receives nothing. No history rewriting outside
  an unmerged `loop/*` branch.
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
- **Batch cap:** ≥ 3 unreviewed entries in REVIEW-QUEUE.md → stop the loop
  (ScheduleWakeup `stop: true` if in /loop) and post the digest (step 9).
  Human review is the throttle — don't run ahead of it.
- **Sync `agent-dev`:** create it at main HEAD if missing. If the queue has
  no unreviewed entries (all merged/discarded by Florian), reset it to main.
  Otherwise, if main gained commits, merge main into agent-dev; a conflict
  here means human work and loop work collided — stop and tell Florian
  instead of resolving on his behalf.
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
`agent-dev` and record the merge-commit hash — that hash is the unit of
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

### 9. Loop control + human digest

Running under /loop: ScheduleWakeup (~1200 s idle pacing; the same /loop
prompt) unless a stop condition hit — backlog empty, batch cap reached, or
two consecutive blocked iterations. **On every stop, and on the `review`
arg:** post the "what lately was done" digest as the final message — per
queued item: title, what/why, diffstat, verification evidence, fit verdict,
flags/dependencies; plus anything `[blocked]` and what the loop learned.
Batch review then works on `agent-dev`:

- take everything: `git merge --no-ff agent-dev` (on main)
- reject one item first: `git revert -m 1 <merge-commit>` on agent-dev,
  then merge
- afterwards mark entries `[reviewed]` (or delete them) — the next
  iteration's sync step resets agent-dev onto main and continues.
