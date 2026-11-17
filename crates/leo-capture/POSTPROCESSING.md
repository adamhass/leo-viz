# Optional schedule postprocessing

Enable the shared stage with `"postprocess": {}` in a solver configuration.
The default is `null` (disabled). It works after independent paths, coordinate
improvement, Lagrangian optimization, and the historical optimizers. It never
changes their multiplier updates, sampled bounds, or iteration history.

The stage operates on the selected, polygon-verified plan:

1. Freeze the set of **missed job/deadline requests** in that original plan.
   Future deadlines and jobs excluded from the run remain outside its scope.
2. Prune globally redundant captures, testing later captures first and removing
   them one at a time. Preserve every assessed request's unique polygon coverage,
   including incomplete mosaics that currently earn zero reward. Preserve met
   deadlines and reward. A 1e-10 relative area tolerance is measured against the
   start of the pruning pass, so errors cannot accumulate over many removals.
3. Find feasible captures in **all gaps** between retained captures, including
   the source/first-capture and last-capture/sink gaps. Two topological passes
   label graph reachability from the preceding anchor and to the following one.
   This works through compressed waiting hubs; no direct A->M->C edges are needed.
4. Greedily insert captures adding polygon area to the initially missed requests.
   Rank by new area times `priority_base^priority`. Recompute marginal coverage
   against the whole constellation after every insertion. A lazy priority queue
   uses monotonically decreasing area bounds; only the affected satellite's gap
   labels need rebuilding. Newly completed requests stop driving insertions.
   Incidental coverage of other requests is still retained and evaluated.
5. Prune once more, removing captures made redundant by the inserted strips.
6. Independently verify the resulting graph paths, precise maneuvers, and polygon
   reward. Publish it only if True Reward and met-deadline count do not regress
   beyond numeric reward tolerance; otherwise keep the original selected plan.

Pruning and refill use full polygons, including holes and separate chart pieces.
They do not use the optimizer's sparse coverage witnesses. Request unions remain
available even after completion. Reported reward nevertheless follows the same
archive-order completion tolerance as the independent verifier.

An insertion may have zero immediate True Reward while adding useful coverage
to a completion-only mosaic. Subsequent insertions, including on other satellites,
can complete it. These are greedy insertions, not an exhaustive search over strip
combinations; an early choice can block a better combination. Retained captures
are fixed during refill. This is one prune/refill/prune pass, not a proof that all
possible improvements or globally minimum capture sets have been found.

## Configuration and saved schedules

```json
"postprocess": {
  "max_insertions": 10000
}
```

`max_insertions` bounds accepted insertions, not polygon evaluations. It defaults
to 10,000 and accepts 1–1,000,000. Reports distinguish hitting the limit from
exhausting feasible candidates with positive marginal area.
Numerical-only marginal area gains are filtered using the same area tolerance.
The refill stop reason describes the queue before the final pruning pass; the
last removals may open further opportunities for a subsequent postprocessing run.

A normal solver run applies the configured stage after choosing its optimizer
winner. To process an existing saved schedule without rerunning its optimizer:

```sh
cargo run --offline --release --manifest-path crates/leo-capture/Cargo.toml \
  --bin leo-schedule -- --postprocess \
  crates/leo-capture/scenarios/worldview60-unique-paths-postprocess-24h.json \
  artifacts/scheduling-solutions/worldview60-unique-paths-coordinate-20261117/schedule.json
```

Use a new output directory. The original schedule and report are preserved.
The input must share the workload, horizon, epoch and observation model; saved
captures are matched by physical identity and validated against the graph.
Saved-only runs report method `postprocess_saved_schedule`, no optimizer
iterations, and the source schedule in `config.incumbent_schedule`.

## Reports and playback

`report.postprocess` contains the targeted `(job, slot)` requests, removals before
and after refill, insertions, candidate/evaluation counts, stop reason, acceptance
decision, and verified before/after metrics. `postprocess_s` measures this stage
separately from optimizer `search_s`; `verification_s` includes the original and
postprocessed plan checks. Polygon-cache preparation is shared. Comparison-matrix
rows carry the same postprocessing information and settings.

The resulting `schedule.json` is a normal schedule: export it with
`leo-schedule-playback` and refresh the visualizer's schedule dropdown. The final
report drives its deadline outcomes and AOI colors.

Implementation: `solver/postprocess.rs`, `postprocess/coverage.rs` and
`postprocess/gaps.rs`. Regression tests cover complementary strips, incomplete
mosaics, global duplicates, separate deadlines/charts, compressed waiting hubs,
source/sink gaps, insertion limits and opt-in behavior. The archive integration
test exercises saved-only processing and the three common-reward optimizers.

## Saved WorldView-60 run, 2026-11-17

Applied directly to the 86.89% aligned coordinate schedule, with its same 53,000
jobs, 2,334 due deadlines, 60 satellites, graph and c = 2 reward. No optimizer was
rerun. The original plan's metrics reproduced exactly before postprocessing.

| Metric | Original | Postprocessed |
| --- | ---: | ---: |
| True Reward | 1,609,613.65 | 1,883,333.74 (+17.01%) |
| Met deadlines | 2,028 | 2,229 |
| Met % | 86.89% | 95.50% |
| Missed deadlines | 306 | 105 |
| Captures | 6,646 | 4,486 |
| Repeated job coverage, km² | 399,361.31 | 222,958.97 (−44.17%) |

The first pass removed 1,999 captures, refill inserted 222, and final pruning
removed another 383. There were 15,473 initially feasible refill candidates and
22,914 marginal evaluations. The insertion cap was not reached. Core postprocessing
took **6.05 seconds**; loading, polygon-cache preparation, initial verification and
final verification brought the saved-only run to **55.21 seconds**. Timing is one
measurement on an active workstation. Source/output paths, provenance, validation
and detailed measurements are in
[`worldview60-postprocess-result.json`](scenarios/worldview60-postprocess-result.json).
