# Plan: tuning a world's dynamics

Temporary: delete when step 5 ships; ARCHITECTURE.md decision 40 onward keep the record.

**The goal (owner, 2026-10-29).** Ask an agent for a behaviour ("foxes hunt more chickens,
but chickens don't all die") and have it get there by reasoning and measuring, not by hours
of blind tweaking. Agents read the rules language well; what they lacked was the middle zoom
level (rates, causes, trends), goals they can check, cheap experiments, and the arithmetic
of ticks and chances. Everything here is an author feature first: any pack author uses it,
the agent is its heaviest user.

**Decisions taken (owner: "go with the recommended").**
- Goals extend the scenario format (`expect` over time, several seeds), so a goal and a
  regression test are the same file.
- The agent may change consts and rule structure; a structural change needs a written
  prediction first and is named in its report.
- Autonomy: the agent checks in once, after the goal is written as properties and the
  baseline is explained, then works alone within a budget (experiments or time) and
  reports.
- No automatic optimizer for now: blind search finds brittle, seed-fitted numbers with no
  explanation. Sweeps plus reasoning instead.

## Steps

1. **`wmc observe`** (done, decision 40): alive over time, deaths by cause, needs, states,
   last results, sampled decisions and rules that fire; summary + long CSV.
2. **Goals over time** (done, decision 41): `expect always|never|eventually COND`,
   `expect mean METRIC`, `expect mean COUNTER K per day` (a rate), each over `from T to T`;
   `eaten K by K2`, `died K of NEED`; `seeds N` + `pass M` (`--seeds`, `--pass`). One
   runner, `sim::run_checks`, for `wmc scenario`, the docs' examples and the fuzzer.
3. **`wmc lab`** (done, decision 42): a scenario's goals for every combination of
   `--set NAME=a,b,c` values, on each seed, `--jobs` runs at once (in-process threads, a
   world each on one thread, not processes); a run stops at its first sure failure unless
   `--full`; per variant its verdict, per expectation its seeds and the spread of its key
   number; `--csv`. `--set NAME=V` sets a const for any command. No pruning of hopeless
   variants yet (it would make the set of runs depend on timing).
4. **The skill** (done, decision 43): `.claude/skills/world-tuning/`: `SKILL.md` (the tools
   by zoom level and the protocol: goal as properties, baseline story, one check-in,
   predict, sweep, explain every miss, confirm with a margin, lock in as a test, report and
   keep the lesson; a budget), `references/arithmetic.md`, `references/cookbook.md`
   (snippets compile), `references/lessons.md`, `references/notebook.md`; `tuning/` holds
   the goals and notebooks.
5. **`wmc rates`.** Each kind's numbers in day units, from the compiled rules.

Probes (tiny drawn-map scenarios that measure one mechanism, like one fox in a pen with N
chickens) need no new tool: steps 1 and 2 make them measurable; the skill teaches them.
