# Comparable scheduling experiments

For optional upstream refinement of job deadline phases, see
[Cadence alignment](CADENCE-ALIGNMENT.md).

For AOI-aligned full-strip candidates and optional task clustering, see
[Strip graph construction](STRIP-GRAPHS.md).

The current workflow separates three inputs:

1. **Workload/timing variant:** jobs, next deadlines and their resulting graph.
2. **Reward model:** geometric area multiplied by `c^priority`.
3. **Optimizer:** independent paths, coordinate improvement or Lagrangian relaxation.

The experiment runner loads a workload's graph and coverage witnesses once, runs
every optimizer under every requested reward model, and evaluates every selected
plan under **all** those reward models. No optimizer-specific prices enter the
published true reward. The old strict-priority experiments remain available as
explicit `legacy_priority` / `legacy_independent` modes outside this comparison.

## Job reward contract

For a due request `r` (one job and one acquisition deadline), let:

- `A[r]` be its full geometric AOI area in km², from the same equal-area charts
  used for polygon verification; metadata/source area estimates are not used.
- `U[r]` be the union of all eligible acquired polygons for this request, clipped
  to the AOI and summed across its chart pieces and all satellites.
- `p[r]` be the saved numerical job priority.

The full reward is `A[r] × c^p[r]`. With `c = 2`, adjacent priorities differ by
exactly a factor of two. For priorities 1–4, the multipliers are 2, 4, 8 and 16.
The existing WorldView workload uses priorities 1–5, which are preserved; its
highest multiplier is 32. `c = 1` provides an area-only comparison.

Each job has an independently editable `acquisition.reward_mode`:

| Mode | True reward for the request |
| --- | --- |
| `unique_area` | `area(U[r]) × c^p[r]`, capped at the full reward |
| `complete_observation` | Full reward if the request is complete, otherwise zero |

Completion retains the existing one-part-per-million geometric tolerance.
**Met %** always counts fully covered due requests, including for partial-credit
jobs. Reward and Met % therefore measure different things and need not rank
plans in the same order. No-due horizons have zero reward and null percentages.

New generated jobs store an explicit reward mode: independent crops use
`unique_area`, mosaics use `complete_observation`. Existing presets without the
field use those same defaults based on fulfilment. The job inspector exposes an
override, so a mosaic can earn partial credit or an independent job can require
completion. Changing fulfilment does not override an explicit reward setting.
New saved workload files use schema 6; older versions continue to load.

Reward is deduplicated **per job and deadline**, including repeated acquisitions
by the same or different satellites. Separate jobs are separate obligations: one
physical capture that genuinely serves two jobs can earn both rewards. A later
deadline needs fresh eligible observations and earns its own reward. Deadlines
beyond the planning horizon and unused graph opportunities receive no reward or
failure record in this experiment. No pre-epoch imagery is credited.

## Optimizers and independent evaluation

Any optimizer can optionally pass its selected plan through
[polygon pruning and missed-deadline refill](POSTPROCESSING.md). This stage is
disabled by default and reported separately from optimizer search. It can also
process an existing saved schedule without rerunning the optimizer.

- **`independent_paths`:** solve an additive maximum-reward path for each satellite
  using the common area/priority weights. It can overvalue repeated coverage in
  its proposals; final evaluation never counts that coverage twice.
- **`coordinate_improvement`:** start from those independent paths and replace
  satellite paths in turn, pricing coverage still missing after removal of the
  satellite's old path. Replacement paths earn each positively priced coverage
  witness at most once, including repetitions within the same satellite path.
  By default, accept only improvements in polygon-verified
  reward, or equal reward with fewer captures.
- **`lagrangian`:** start from the same candidates, relax coverage requirements
  into prices and iteratively solve the satellite subproblems. Partial-credit
  jobs use independently rewarded area witnesses; completion-only jobs require
  all their witnesses for a request reward. Coordinate improvement repairs each
  proposal. Polygon reward also selects raw/repaired incumbents. Intermediate
  incumbent plans are retained for independent polygon verification.

Replacement search keeps up to `solver.replacement_path_labels` coverage histories
at each graph node (default **4**, supported range **1–16**). Each history carries
a compact bitset of its acquired witnesses. A capture earns only newly covered
witness prices; fully repeated captures are skipped, while complementary strips
can overlap. Capture and skip choices remain distinct through waiting and maneuver
nodes. Identical coverage states merge; different states compete for the bounded
frontier. A final pass removes captures made redundant by later strips, preserving
all positively priced sampled coverage. Polygon acceptance still checks the whole
replacement against contributions from every other satellite.

This bounded search is a heuristic, not an exact maximum-unique-coverage path
solver. One satellite can still have exponentially many relevant coverage
histories. The fixed label limit controls cost; increasing it preserves more
alternatives at a higher runtime/memory cost. Witness coverage remains an
approximation: a strip with no new witness can still have new polygon area.

Only coordinate **replacement proposals** use this search. Independent baseline
paths and raw Lagrangian subproblems retain the exact additive DAG oracle. Their
unmodified counts and values supply multiplier updates and sampled dual bounds;
coverage-aware or pruned paths never masquerade as exact dual subproblem solutions.
Reports include `acceptance.replacement_search` with the method, label limit and
path-search time (a subset of total `search_s`).

The isolated 2026-11-17 comparison used the saved aligned 24-hour graph (60
satellites, 155,876 actions, 8,423,392 arcs), c = 2, exact polygon acceptance and
two coordinate sweeps from the identical independent plan. The new search kept
four histories per node. Neither measurement run performed Lagrangian iterations.

| Metric | Additive replacement control | Unique-coverage replacement |
| --- | ---: | ---: |
| True reward | 1,591,017.76 | 1,609,613.65 (+1.17%) |
| Met deadlines / 2,334 | 1,978 | 2,028 |
| Met % | 84.75% | 86.89% |
| Repeated job coverage, km² | 416,884.01 | 399,361.31 (−4.20%) |
| Captures | 6,577 | 6,646 |
| Search time, seconds | 5.76 | 19.55 |

Coverage-aware path proposals took 9.70 seconds within the new search time.
Observed process memory rose from 708.7 MB to 742.5 MB. These are single-run
measurements; final archive verification is separate and was subject to concurrent
compilation. Both plans passed graph-path, maneuver and independent polygon
verification. The control reproduced the previous coordinate result exactly.
Repeated coverage is summed per job and deadline; total capture count is not a
duplicate count. Raw/retained paths can still contain repetitions, and useful
strips may overlap, so unique replacement pricing does not imply zero repeats
in the final constellation plan.

Configuration: [`worldview60-unique-paths-coordinate-24h.json`](scenarios/worldview60-unique-paths-coordinate-24h.json).
Results and provenance: [`worldview60-unique-paths-result.json`](scenarios/worldview60-unique-paths-result.json).

Witnesses represent polygon-component areas, rather than each receiving the same
area irrespective of their size. They still approximate coverage; a covered point
does not certify its entire component. The verifier reconstructs union polygons
from archived crops, checks path/maneuver feasibility and computes true reward.
Final candidate selection maximizes that true reward, then uses total met
deadlines and fewer captures to break exact ties. Strict priority ordering is not
used by any of the three comparison optimizers.

### Exact acceptance and cost accounting

`solver.acceptance` defaults to `exact_polygon`. `sampled` reproduces the earlier
acceptance heuristic for controlled comparisons; it still receives a final
polygon verification and can fall back to the independent plan.

Exact acceptance loads and AOI-clips the archived crop polygons once per workload,
in parallel by satellite. Each optimizer maintains selected crop IDs per deadline.
For a proposed satellite replacement it recomputes only deadlines touched by the
old or new path, including contributions from every other satellite. Rejected
proposals leave the current state intact. Crop unions use deterministic archive
order, the same chart pieces, holes, collection eligibility and completion
tolerance as the final verifier. A bounded cache (32 selected-crop combinations
per deadline) reuses repeated evaluations. It is reset for each optimizer run.

This is exact relative to the saved polygon geometry and existing floating-point
tolerance. It does not optimize continuous pointing offsets, generate mosaic
templates or change graph proposals: witness prices still generate the candidate
paths. Exact acceptance can prevent harmful replacements but cannot discover a
replacement that the path heuristic never proposes.

Reports separate shared `polygon_cache_prepared_once_s` from each optimizer's
`search_s` and final `verification_s`. `acceptance.polygon_evaluation.evaluation_s`
is a subset of search time, including initial-plan evaluations and incremental
replacement checks; it must not be added again. Request recomputation/cache-hit,
union and accepted/proposed-path counts accompany it. The final verifier rereads
the archives independently; the run fails if its reward disagrees with the cached
incumbent evaluation. Input preparation and polygon-cache times occur once per
comparison, not once per row.

The Lagrangian lower/upper values are explicitly named **sampled** rewards/bounds.
They are not true reward and do not certify an optimality gap for the polygon
problem. The sampled lower bound tracks the best sampled score seen, independently
of the polygon incumbent; only sampled values enter price updates and stopping
conditions. A polygon reward can exceed the sampled upper bound without error. The coordinate/Lagrangian methods include their starting baseline as a
candidate; their search time includes that work. Polygon verification is timed
separately. These methods are heuristic and may not find a better plan than their
initial candidate.

## Running a comparison

From the repository root, with the saved ongoing graph available:

```sh
cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-schedule-compare -- \
  crates/leo-capture/scenarios/worldview60-reward-comparison-24h.json
```

The original scenario explicitly preserves `acceptance: "sampled"`. The paired
`worldview60-acceptance-sampled-24h.json` and
`worldview60-acceptance-exact_polygon-24h.json` scenarios compare the two acceptance
modes with identical inputs and search budgets. Run them sequentially with the
same release binary to measure cost.

The scenario compares `area_only` (`c = 1`) and `area_priority_c2` (`c = 2`) using
all three optimizers, 30 Lagrangian iterations, two coordinate sweeps and the same
3 × 3 witness grid. Each job's partial-credit policy is identical across runs.
To test another multiplier, add another entry to `reward_models`. Select a new
`output` directory for another run; existing outputs are not overwritten.

Paths resolve relative to the scenario file. Outputs are:

```text
comparison.json
<workload>/<reward-model>/<optimizer>/report.json
<workload>/<reward-model>/<optimizer>/schedule.json
```

`comparison.json` records graph/workload hashes, priority and reward-policy counts,
due counts, met/missed percentages, true reward, all cross-evaluations, captured
segments and execution times. Each individual report also records candidate
scores, sampled scores and iteration diagnostics. `INCOMPLETE` marks interrupted
runs. Completed plans can be exported through `leo-schedule-playback` as before.

For a single run, add these fields to a regular `leo-schedule` configuration:

```json
{
  "optimizer": "lagrangian",
  "reward": { "priority_base": 2.0 },
  "iterations": 30,
  "acceptance": "exact_polygon",
  "repair_sweeps": 2,
  "replacement_path_labels": 4
}
```

`--baseline` uses the same configured reward model with independent paths. Old
scenario files now explicitly name their legacy optimizer to preserve their
historical behavior; their reports are not common-objective comparisons.

## First 24-hour comparison (sampled acceptance)

The saved randomized workload has 26,477 partial-credit jobs and 26,523
completion-only jobs, with 2,334 deadlines due during the first day. All three
optimizers retained the independent plan as their best polygon-verified candidate
under each reward model. Coordinate/Lagrangian search did not demonstrate a true
reward improvement at this witness density and iteration budget.

| Optimized for | Met deadlines | Met % | True reward, area-only | True reward, c = 2 |
| --- | ---: | ---: | ---: | ---: |
| Area only | 1,049 | 44.94% | 85,472.19 | 1,073,322.32 |
| Area × 2^priority | 1,063 | 45.54% | 79,812.68 | 1,110,875.30 |

Rows represent two selected plans, each shared by the three optimizer runs.
Compare rewards **within a column**, since the columns have different units of
priority weighting. The complete six-run matrix, misses, timings, candidate
identities and provenance are in
[`worldview60-reward-comparison-result.json`](scenarios/worldview60-reward-comparison-result.json).
The whole comparison took 148.6 seconds, including shared input preparation and
all final polygon checks, excluding compilation and graph generation.

## Polygon acceptance comparison

The paired rerun uses the same 24-hour graph, 53,000 jobs, 2,334 due deadlines,
reward policies, three-by-three witnesses, two repair sweeps and 30 Lagrangian
iterations. Both acceptance modes ran sequentially using the same release binary
and four workers. Every sampled-control schedule matches the earlier comparison.
No graph, job timing, pointing choices or mosaic templates changed.

Both coordinated methods now improve true reward. Every exact incumbent was
non-decreasing under the independent archive-based polygon verifier, whose final
rewards agreed with the cached acceptance evaluation. All plans passed graph-path
and precise maneuver checks.

| Reward model | Optimizer | True reward | Gain over independent | Met / 2,334 | Met % | Missed |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Area only | Independent | 85,472.19 | +0.00% | 1,049 | 44.94% | 1,285 |
| Area only | Coordinate | 90,925.37 | +6.38% | 1,238 | 53.04% | 1,096 |
| Area only | Lagrangian | 91,281.44 | +6.80% | 1,258 | 53.90% | 1,076 |
| Area × 2^priority | Independent | 1,110,875.30 | +0.00% | 1,063 | 45.54% | 1,271 |
| Area × 2^priority | Coordinate | 1,171,327.91 | +5.44% | 1,229 | 52.66% | 1,105 |
| Area × 2^priority | Lagrangian | 1,174,949.49 | +5.77% | 1,244 | 53.30% | 1,090 |

Times below are measured wall-clock seconds; each cell is one run, so small
baseline differences reflect runtime variation. Polygon evaluation is included
in search, and final verification is timed separately.

| Reward model | Optimizer | Sampled search | Exact search | Exact polygon evaluation within search | Sampled search + verification | Exact search + verification |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Area only | Independent | 0.05 | 0.07 | 0.00 | 15.00 | 14.63 |
| Area only | Coordinate | 0.42 | 12.04 | 11.60 | 15.75 | 25.52 |
| Area only | Lagrangian | 24.05 | 228.16 | 210.45 | 57.12 | 255.37 |
| Area × 2^priority | Independent | 0.06 | 0.04 | 0.00 | 15.10 | 14.96 |
| Area × 2^priority | Coordinate | 0.44 | 14.24 | 13.77 | 17.26 | 30.80 |
| Area × 2^priority | Lagrangian | 27.78 | 310.30 | 285.34 | 62.69 | 341.39 |

The exact comparison additionally prepared **154,813 eligible crop polygons in
6.59 seconds**, shared across all six runs. Graph/witness preparation took 20.88
seconds in the exact run and 19.94 seconds in the control. Including shared
preparation and all verification, the full matrix took **713.38 seconds** with
exact acceptance versus **206.87 seconds** with sampled acceptance. Compilation
and the original graph construction are excluded.

For `c = 2`, coordinate improvement delivers a **5.44% reward gain** for 30.80
seconds of search plus verification, compared with 17.26 seconds using sampled
acceptance. Lagrangian search reaches **5.77% above independent**, but takes
341.39 seconds. Its additional gain over coordinate improvement is **0.31%**
(15 more deadlines met). Area-only results show the same pattern: a 6.38% gain
from coordinate improvement and 6.80% from Lagrangian, with the latter adding
only 0.39% beyond coordinate improvement.

Coordinate improvement therefore offers the better runtime/reward tradeoff in
this experiment. Exact acceptance fixes harmful sampled replacements; the
remaining limitation is that candidate generation still follows witness prices.
These results are not an optimality claim or a test of alternative mosaic layouts.

The portable paired results, all cross-model rewards, missed percentages, cost
counters, binary hash and artifact locations are saved in
[`worldview60-polygon-acceptance-result.json`](scenarios/worldview60-polygon-acceptance-result.json).
Full reports and schedules are under
`artifacts/scheduling-solutions/worldview60-acceptance-{sampled,exact_polygon}-20261116/`.
The bundled visualization schedule is unchanged.

## Upstream workload timing design

`workloads` is a list of named variants, each with a provenance `timing_policy`
label and its own `graph_directory`. The earlier reward scenarios retain the
randomized workload. The optional [`leo-job-align` step](CADENCE-ALIGNMENT.md)
now supplies geographic phase variants without changing recurrence periods.

A timing designer produces another preset and graph, then adds that
variant to the same experiment matrix. It must regenerate or correctly rebuild
eligibility because changing deadlines changes collection windows and graph crop
records. Relabeling the original graph is insufficient. The runner requires the
same epoch, constellation and observation model across timing variants.

If a variant changes how many deadlines fall inside the horizon, compare its due
counts and available reward alongside Met % and true reward. Different demand
sets are not interchangeable denominators. Cadence alignment preserves the
per-cadence deadline multiset and hence all deadline counts, but can still change
which AOI areas and priorities are due on a given day.

## Tests

```sh
cargo test --offline --manifest-path crates/leo-capture/Cargo.toml -j 2
```

Tests cover proportional area/priority rewards, mixed partial/completion policy,
duplicate-free witness scoring, graph-feasible coordinate improvements, sampled
dual bounds on a small known optimum, persistence, and the full experiment matrix
on a freshly generated graph with independent polygon verification. Exact-acceptance
regressions cover rejecting sampled gains that leave mosaic gaps, accepting real
gains, duplicate coverage, holes, multiple chart pieces, independent deadlines,
proposal rollback and true rewards that exceed sampled dual bounds.
