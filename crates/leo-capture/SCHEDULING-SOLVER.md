# Experimental scheduling solver

The current default visualizer plan is the 58.57% independent-path baseline for
the redesigned ongoing workload. A portable copy is bundled with the app; the
[replay guide](../../docs/schedule-playback.md) describes loading it, regenerating
its graph and exporting new schedules. Earlier experiments below remain recorded
with their original workloads and objectives.

`leo-schedule` selects one feasible acquisition path per satellite from the saved
graphs. It coordinates shared coverage and checks selected crops with polygon
unions before reporting met acquisition deadlines. It never modifies the saved graph or
workload. There is no new dependency or external optimization service.

## Opportunities, acquisition deadlines and ongoing jobs

- An **opportunity** is an optional graph capture. Skipping one is not a failure.
- An **acquisition request** is the coverage required by one job in one cadence
  window. It has its own deadline and may need several captures.
- A **planned missed deadline** is an acquisition request due within the horizon
  whose coverage is incomplete in the selected plan. This is a planning verdict,
  not an observed failed imaging attempt or a forecast of weather.
- A **job** is ongoing. One missed request neither fails the job nor reduces the
  value of fulfilling its other requests. No job-failure or job-drop state is
  created by this solver.
- A deadline beyond the horizon is **not yet due** and cannot count as missed.

The primary objective is the number of acquisition deadlines met at each job
priority, compared lexicographically from highest to lowest priority. Each cadence
window earns independent credit. Fractional coverage is only a tie-breaker.

The original archived workload has phase zero for every job. Its deadlines are
`epoch + phase + (slot + 1) * cadence`; phase is the first window's start, not an
immediate deadline. For the saved first 24 hours:

| Cadence | Ongoing jobs | Due acquisition deadlines | First deadline after epoch |
| --- | ---: | ---: | --- |
| 4 hours | 13,170 | 79,020 | 4 hours |
| 24 hours | 13,264 | 13,264 | 24 hours |
| 7 days | 13,184 | 0 | 7 days |
| 30 days | 13,382 | 0 | 30 days |

Thus 26,566 jobs have no deadline in this horizon; their open requests remain
pending. The denominator of the due-deadline metric is 92,284, not the number of
graph opportunities or the number of jobs. These weekly/monthly exclusions also
applied to the original run. This saved workload is a synchronized cold start;
the solver does not invent staggered job ages or change archived phases.

```sh
cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-schedule -- crates/leo-capture/scenarios/worldview60-deadlines-4h.json
```

The example uses all 60 satellites and the first four hours of the existing
24-hour graph. `scenarios/worldview60-deadlines-24h.json` runs the full day.
Choose a new `output` directory for a repeat run. The horizon must end on a graph tick. Existing output
directories are rejected. `report.json` and absence of `INCOMPLETE` mark a
completed result; `schedule.json` contains satellite IDs and selected capture
action IDs, ticks and modes. Empty capture lists mean an idle satellite.

Version-2 reports include `due_acquisition_deadlines`, `planned_met_deadlines`,
`planned_missed_deadlines`, `planned_miss_rate_percent`, partial/no-coverage
breakdowns, pending requests and a cadence breakdown. With no due deadlines, the
miss count is zero and the percentage is null (not applicable). Future work has
no search reward in this prefix experiment; look-ahead planning and carrying
imagery between successive planning runs are not implemented yet.

To schedule only existing daily jobs, run
`scenarios/worldview60-daily-only-24h.json`. Its optional
`job_cadence_hours: 24` filter selects the 13,264 archived jobs whose cadence is
exactly 24 hours; omitting the field includes every job. The filter preserves job
indices, graph connectivity, priorities, phases, illumination rules and coverage
requirements. It changes only which jobs receive search rewards and deadline
assessment. Captures can incidentally cover other jobs. Other cadence groups are
excluded from this experiment, not converted into daily jobs or counted as
missed/pending requests. Reports distinguish `ongoing_jobs` in the archive,
`jobs_in_scope` and `jobs_excluded_from_run`; due, pending and cadence breakdowns
refer only to the selected scope.

## Randomized ongoing workload

`worldview60-ongoing-solver-24h.json` uses all 53,000 redesigned jobs with
independent next deadlines uniform from five hours to each job's cadence. This
uses the one-second graph with precise maneuver durations and 60 satellite paths.
Only deadlines closing in the first day earn search reward. Future deadlines are
not failures; every job continues after both met and missed requests.

The graph configuration retains actions serving at least one due deadline and
also records incidental future-job crops on those actions. No pre-epoch imagery
is credited. This can understate coverage for jobs whose real ongoing collection
window would already contain historical imagery. See `JOB-DESIGN.md` for deadline
assignment and the distinction between the elapsed-time buffer and usable passes.

### Measured ongoing first-day run

The graph built on the 2026-11-13 00:00 UTC design epoch contains 141,773 actions,
283,666 nodes and 12,030,289 edges across 60 satellites, taking 587.2 seconds to
construct. The solver took 35.7 seconds. Of 53,000 ongoing jobs, 2,334 have a
first-day deadline and 50,666 are not due. All deadlines are at least five hours
out (observed minimum 5.0249 hours); subsequent deadlines keep the original cadence.

| Verified candidate | Met | Met % | Missed | Missed % | Priority-5 met |
| --- | ---: | ---: | ---: | ---: | ---: |
| Selected strict-priority plan (`after_priority_5`) | 309 | 13.24% | 2,025 | 86.76% | 294 / 457 |
| Independent-path baseline from the same run | 1,367 | 58.57% | 967 | 41.43% | 254 / 457 |

The selected plan has 3,538 captures; 116 missed deadlines have partial coverage
and 1,909 have none. It is selected because the objective values any increase in
highest-priority completions above any number of lower-priority completions. The
baseline is a polygon-verified alternative, not the persisted selected schedule.

The approximation also matters: later search stages retain 385 sampled
priority-5 completions while reducing actual polygon-complete requests from 294
to 262. Their geometric loss causes the final verifier to prefer the earlier
priority-5-only candidate. Sparse witness preservation does not ensure preservation
of a complete mosaic. This run is evidence about the current heuristic and strict
priority policy, not a constellation capacity limit.

| Collection mode | Due | Met | Missed | Missed % |
| --- | ---: | ---: | ---: | ---: |
| Independent crops | 1,183 | 168 | 1,015 | 85.80% |
| Similar local solar time | 577 | 69 | 508 | 88.04% |
| Close in elapsed time | 574 | 72 | 502 | 87.46% |

An optimistic union of every candidate gives complete coverage for 1,977 requests
(84.70%); the other 357 have no eligible candidate before their deadlines. No
request has only partial candidate-union coverage. This separates lack of access
from the 1,668 additional selected-plan misses, but does not separate conflicts
from priorities or search quality. The cutoff cannot guarantee usable passes,
and no historical imagery is credited even when a job's collection window begins
before the epoch.

Independent validation checked every retained action, all 350,193 crop intervals,
and all 60 selected paths. The plan includes 1,922 pointing changes, with a
minimum changed-pointing gap of three seconds. All timing and metric partitions
passed. See [`scenarios/worldview60-ongoing-result.json`](scenarios/worldview60-ongoing-result.json)
for the reproducible record and artifact paths. The full library passed 69 tests
(three existing scale tests ignored), with 20 scheduling tests rerun after the
report changes; the application also passed `cargo check`.

## Daily-only experiment (v2)

One run of `scenarios/worldview60-daily-only-24h.json` used the original 24-hour
graph and all 60 satellites, selecting only the 13,264 existing daily jobs. It
retained their priorities and daylight/solar-time constraints, with six iterations
per priority, 3 × 3 witnesses and one repair sweep. The 39,736 other jobs were
outside the objective and deadline assessment. No jobs or graph files changed.

The run took 114.3 seconds excluding compilation. Polygon verification selected
`after_priority_1`, containing 2,012 captures. All 13,264 daily jobs have one
deadline at the end of this horizon:

| Candidate | Met deadlines | Met % | Missed deadlines | Missed % | Priority-5 deadlines met |
| --- | ---: | ---: | ---: | ---: | ---: |
| Selected priority-aware plan | 1,537 | 11.59% | 11,727 | 88.41% | 822 / 2,631 |
| Independent-path baseline from the same run | 2,214 | 16.69% | 11,050 | 83.31% | 451 / 2,631 |

Of the selected plan's misses, 1,998 have partial coverage and 9,729 have no
coverage. These are planned missed acquisition deadlines; the jobs remain ongoing.
The strict priority objective prefers the selected plan despite its lower total
completion. This experiment still performs poorly and does not establish a fleet
capacity limit or prove that the remaining daily requests are impossible.

The record is
[`scenarios/worldview60-daily-only-result.json`](scenarios/worldview60-daily-only-result.json).
Full artifacts are in
`artifacts/scheduling-solutions/worldview60-24h-daily-only-v2/`. All 61 non-ignored
unit tests passed (three existing scale benchmarks ignored). Independent checks
verified all 60 maneuver sequences, the daily-only scope, per-priority deadline
counts and metric partitions for every candidate.

## Measured deadline-objective run (v2)

The full 24-hour run took 117.3 seconds including preparation, search and polygon
verification. It kept all 53,000 jobs ongoing, counted 92,284 due acquisition
deadlines, and kept 26,566 open weekly/monthly requests outside the failure count.
The selected plan contains 13,193 captures and meets 12,475 due deadlines; 79,809
are planned misses (86.48%). Of these misses, 19,381 have partial coverage and
60,428 have no coverage. The completion criterion remains full requested area in
each cadence window; an unused opportunity creates no failure record.

Priority-5 met deadlines increased from 5,190 in the historical v1 selected plan
to 6,880 in v2. Total met deadlines decreased from 12,776 to 12,475 under the
strict priority objective. Correcting the unit of optimization does not establish
good solution quality or prove that missed deadlines are unavoidable.

The v2 independent-path baseline uses equal reward per acquisition request
(v1 used equal reward per whole job) and meets 18,684 deadlines. It is therefore
also an updated baseline, not an unchanged controlled comparison.

The saved report is
[`scenarios/worldview60-deadlines-result.json`](scenarios/worldview60-deadlines-result.json).
All 60 non-ignored capture tests passed, including 12 solver tests. Independent
checks verified all 60 maneuver sequences, deadline-count partitions and the
weekly/monthly exclusions. The original graphs, assignments and v1 outputs are
preserved.

## Historical v1 runs: superseded whole-job objective

The first prototype incorrectly prioritized jobs meeting every deadline over the
whole day. The following records are retained for comparison, but their whole-job
counts are not job successes/failures and their objective is superseded by v2.
Misses never represented skipped graph opportunities. The correction changes
reward and candidate selection, not the archived acquisition opportunities.

Both runs used all 60 satellites, six iterations per priority, a 3 × 3 spatial
grid per chart piece and one coordinate sweep. Full records are in
[`scenarios/worldview60-solver-result.json`](scenarios/worldview60-solver-result.json).
Times exclude compilation and include indexing, search and polygon verification.

| Horizon | Capture choices | Wall time | Verified priority-5 whole jobs, baseline → selected | All verified whole jobs, baseline → selected | Selected complete due requests |
| --- | ---: | ---: | ---: | ---: | ---: |
| 4 hours | 159,760 | 42.5 s | 592 → 1,098 | 2,890 → 2,269 | 2,269 / 13,170 |
| 24 hours | 973,789 | 146.1 s | 535 → 1,057 | 2,738 → 1,739 | 12,776 / 92,284 |

The v1 baseline used equal per-job witness rewards and independent satellite solves;
it is a reference, not a controlled reproduction of the paper's benchmarks.
Prioritizing whole high-priority jobs reduces aggregate completion in these runs.
A whole job must meet every deadline in the chosen horizon, so whole-job counts
across different horizons are not directly comparable. The full-day run has
26,434 jobs with due deadlines out of the 53,000 saved jobs.

The full-day selected plan contains 15,080 captures. Both plans passed an
independent check of initial maneuvers and every consecutive pair of captures
against the saved quaternion slew law. All 55 non-ignored capture tests passed,
including seven new solver tests; three pre-existing scale benchmarks remained
ignored. Reported process footprints at completion were about 191 MiB and
484 MiB respectively, not peak-memory guarantees.

Search approximation remains material: the full-day winner predicted 1,566
sampled priority-5 whole-job completions, of which 1,057 passed polygon coverage.
Only the latter are credited. In the four-hour run, verification selected the
priority-2 checkpoint over the final checkpoint, preserving two real priority-5
completions that the sampled search failed to distinguish. Adaptive coverage
refinement is a useful next improvement; increasing iterations alone does not
remove this approximation.

## Diagnosing missed deadlines

The candidate-union diagnostic compares a saved plan with the union of **every**
eligible crop, including mutually incompatible and source-unreachable choices.
The latter is deliberately optimistic: an incomplete union proves insufficient
exported geometric opportunities, but a complete union does not prove a feasible
single-job or constellation schedule.

```sh
crates/leo-capture/target/release/leo-schedule --diagnose \
  crates/leo-capture/scenarios/worldview60-solver-24h.json \
  artifacts/scheduling-solutions/worldview60-24h-lagrangian-v1/schedule.json \
  artifacts/scheduling-solutions/worldview60-24h-lagrangian-v1/coverage-diagnostic.json
```

Use a new output filename on repeats. The diagnostic does not change the plan.
It reports coverage distributions, cadence/fulfilment groups and priority groups.
The first result is saved in
[`scenarios/worldview60-coverage-diagnostic-result.json`](scenarios/worldview60-coverage-diagnostic-result.json).

Of 92,284 due requests, **92,069 (99.77%)** have complete candidate-union coverage;
215 (0.23%) remain incomplete, including 179 with no candidate crops. All 79,020
four-hour requests have complete candidate unions. All 215 geometric shortfalls
belong to daily requests. None of this proves that compatible paths can collect
those unions, nor does it establish physical impossibility beyond the finite
candidate model.

The selected plan fully covers 12,776 requests, partially covers 19,448 and leaves
60,060 with zero coverage. Only 956 missed requests reach 99% coverage. Thus the
dominant observed pattern is unserved windows, not tiny residual mosaic gaps.
The other 79,293 misses have enough candidate-union coverage and remain unresolved
between path/resource conflicts, the whole-job priority objective and heuristic
search limitations. Current evidence does not quantify fleet overload.

## Relationship to the paper

Wang et al., *A Lagrangian relaxation-based heuristic algorithm for multiple agile
earth observation satellite scheduling with time-dependent constraint*, European
Journal of Operational Research 332(1), 84–100 (2026),
[doi:10.1016/j.ejor.2025.12.047](https://doi.org/10.1016/j.ejor.2025.12.047).

The publicly accessible publisher preview describes relaxing task uniqueness to
separate satellite subproblems, followed by dual-guided feasible construction and
improvement. The full heuristic section and implementation were not available
during implementation. This code adapts that decomposition architecture; it is
**not a reproduction of LRD-H**, and does not claim its benchmark results.

Our model needs a different coupling constraint: a capture may contribute to
multiple jobs, while a job may need a mosaic across several captures and
satellites. Penalizing repeated job IDs would incorrectly forbid those mosaics.
The search therefore relaxes coverage requirements instead. All selected paths
are feasible in the finite graph; repeated coverage simply earns no additional
completion credit. The per-satellite additive subproblem is solved exactly by a
topological DAG pass, including optional capture/skip and shared transition hubs.

## Search approximation and decomposition

Rebuild the original atlas from the checksum-verified archived preset. Check its
chart IDs and centres against the export. Divide each due job's atlas pieces into
`samples_per_axis` × `samples_per_axis` rectangles and take an interior witness
from every positive-area polygon component of each intersection. Holes, narrow
rivers and disconnected islands are retained during this process. These points
are a search approximation, not a coverage certificate.

A separate witness requirement is created for each cadence slot closing inside
the selected horizon. An action covers a witness only when its eligible crop
contains the point in the matching job, slot and chart. Its witness IDs are
deduplicated; one action can cover witnesses for multiple jobs. Jobs with pending
deadlines outside the prefix receive no reward in this experiment.

Let `z[r]` indicate completion of **one acquisition request** `(job, cadence slot)`,
and `n[c]` be the number of selected captures covering witness `c`. The sampled
optimization has constraints `z[r(c)] <= n[c]` and a scalar stage objective
`sum(w[r]*z[r])`. Different windows of the same ongoing job have independent
variables and rewards; missing one cannot erase credit for another.
Relaxing these constraints with nonnegative prices `lambda[c]` gives:

```text
U(lambda) = sum_r max(0, w[r] - sum_{c belonging to request r} lambda[c])
          + sum_satellites max_path sum_selected_actions sum_{c in action} lambda[c]
```

The DAG oracle maximizes the second term. A projected subgradient update uses
`lambda[c] = max(0, lambda[c] + step * (z[r(c)] - n[c]))`, with a Polyak-style
step based on the current dual value and best feasible sampled objective.
The reported stage lower and upper bounds apply only to that stage's **sampled
scalar objective**, including its protected-request weights. They are not bounds or
an optimality gap for polygon-verified scheduling.

After each dual solve, coordinate improvement removes one satellite's current
coverage, prices uncovered witnesses using dual prices plus normalized job
weights for individual requests, and re-solves that satellite. A replacement is accepted only when the
deduplicated sampled completion score improves. Repeated witnesses within the
replacement cannot fool this acceptance check. Both the relaxed candidate and
the incumbent receive this improvement pass; satellite order rotates between
iterations. The independent additive-path solution is retained as a baseline.

## Priorities and acceptance

Process priority levels from highest to lowest. Within a stage, already met
sampled acquisition requests of higher priority receive a protection weight larger
than the number of due requests; requests at the current tier receive weight one. These weights guide
candidate generation. They are not the final priority policy.

Both sampled incumbents and final verified candidates are ranked lexicographically:

1. Numbers of met acquisition deadlines, by descending job priority.
2. Total fractional request coverage as a tie-breaker.

Consequently, candidate acceptance never substitutes arbitrarily many
lower-priority met deadlines for one higher-priority met deadline. Search is
heuristic and does not prove optimality. A job meeting five of six deadlines earns
five credits and remains active; meeting its sixth deadline earns one more.
Independent-crop partial delivery and unfinished mosaics receive the final
partial-coverage tie-break. This is a deadline scheduler, not a job-admission or
job-termination policy.

## Verification and limits

Keep the baseline and the best sampled incumbent after each priority stage.
Before publishing, verify all distinct candidates in a second streaming pass:

- Require every selected capture to fit on a source-connected graph path.
- Check action IDs, ticks, modes, crop time windows and cadence slots.
- Clip selected crop geometry to its original AOI piece and union by job, slot
  and chart. Count completion only at the baseline's relative area tolerance,
  `1e-6`. Duplicate crops cannot increase union coverage.
- Compare the resulting actual completion vectors, not sampled completion
  estimates, and retain the best verified candidate including the baseline.

The report includes both sampled and actual scores so false positive sampled
completions remain visible. The sample model can miss thin uncovered gaps; more
samples increase preparation cost, and there is no adaptive refinement yet.
Exact verification prevents false completion claims but does not recover every
good schedule overlooked by the sampled search.

The graph's finite-model assumptions still apply: discrete pointing modes and
imaging samples, with no clouds, downlink, storage, processing or energy constraints.
Historical graphs use 30-second captures and rounded maneuver timing; the current
ongoing-workload graph uses one-second captures and precise angular slew durations.
Prefix termination means waiting/skipping to the original sink. No previously
acquired imagery or committed actions are used.

The implementation reuses the archived files without writing a large intermediate
cache, bounds worker count with the existing shared resource limits, checks
process memory, and applies the CPU duty allowance between search iterations.
Preparation and polygon verification can dominate runtime. Only measured runs
should be used to assess scaling.

Tests cover exhaustive small-DAG path comparisons, unreachable and conflicting
captures, duplicate/overlapping crop unions, complementary mosaic pieces,
cadence separation, continued credit after a missed request, future and phased
deadlines, zero-due horizons, strict priority comparison and a two-satellite
reassignment case. Run `cargo test --release --manifest-path crates/leo-capture/Cargo.toml
scheduling::solver`.

## Replaying the independent-path candidate

The `--baseline` option reconstructs and saves exactly the deterministic
`independent_paths_baseline` candidate used in the normal candidate comparison.
It uses the same graphs, spatial witnesses, timing validation and polygon-union
verification, and skips the priority optimization stages. All due requests have
equal normalized coverage weight in this candidate; it is not a globally optimal
maximum-deadline solver. Existing outputs are never overwritten.

```sh
cargo run --release --manifest-path crates/leo-capture/Cargo.toml --bin leo-schedule -- \
  --baseline crates/leo-capture/scenarios/worldview60-ongoing-baseline-24h.json
cargo run --release --manifest-path crates/leo-capture/Cargo.toml --bin leo-schedule-playback -- \
  artifacts/scheduling-solutions/worldview60-ongoing-baseline-24h-20261113/schedule.json \
  artifacts/scheduling-solutions/worldview60-ongoing-baseline-24h-20261113/playback.json.gz
```

This candidate is bundled as the default visualizer replay at
`assets/schedules/worldview60-ongoing-baseline-24h.json.gz`, replacing the sparse
priority-selected plan without deleting it. Its verified 24-hour result is 1,367/2,334 deadlines met
(58.57%), 967 missed (41.43%), and 22,462 capture segments. Red AOIs use the deadline
assessment for this selected plan.
