# tuning/

One folder per goal a world was tuned for, with the `/world-tuning` skill
(`.claude/skills/world-tuning/`):

- `goal.scenario`: the goal as expectations over time (`docs/RULES.md` §14), on several
  seeds; `goal-fast.scenario`: the same on a smaller world or a shorter run, for iterating.
- `notes.md`: the notebook: the properties and why, the baseline, the story of who limits
  whom, each experiment with its prediction and result, the decision and its margin.
- `runs/`: CSVs from `wmc observe` and `wmc lab`; not committed.

A goal that holds also ends up as a test in `scenarios/tests/`, so `make test` keeps it
true.
