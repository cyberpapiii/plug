# Off-Main State

This file records roadmap-relevant work that exists outside `main`. These branches are candidate
future state only and must never be described as current implementation.

| branch / worktree | relationship to `main` | feature area | classification | notes |
|---|---|---|---|---|
| `feat/roots-forwarding` | ahead of `main` by 2 commits, PR #32 open | roots forwarding | merge-ready candidate | adds roots cache, reverse request transport handling, tests, and review fixes; not current truth until merged |
| `fix/subscription-rebind-confidence` | ahead of `main` by 4 commits, ~18k lines / 74 files vs `main` | roots + elicitation + sampling + legacy SSE + OAuth + Stream A checkpoint work | salvageable checkpoint, not mergeable as-is | contains substantial future work but is too large and mixed to count as current state |
| `feat/post-v0-2-upstream-restart-proof` | no diff vs `main` | restart proof | superseded / already merged | branch content is already represented on `main` |
| `feat/daemon-client-session-continuity` | no diff vs `main` in current topology listing beyond separate worktree branch | continuity follow-up | candidate / needs separate review if reopened | keep separate from current truth unless merged |
| older phase branches and worktrees (`phase2*`, `phase3*`, `roadmap-tail-closeout`, `v0-1-stabilization`) | merged history | historical implementation lineage | superseded / historical | useful as archaeology only |

## Off-Main Conclusions

- only `feat/roots-forwarding` appears to be an actively reviewable next-step branch
- `fix/subscription-rebind-confidence` is useful as a source of extractable future work, but it is
  not a truthful project-status source
- merged historical worktrees should not be referenced as current-state proof
