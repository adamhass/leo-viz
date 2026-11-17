# AOI-based full-strip graphs

The original fixed-pointing, one-tick graph builder remains available. An optional
`strips` block selects the new builder. It derives left/right-aligned mosaic
strips from AOI geometry on each actual satellite pass, retaining the configured
along-track looks. Captures have explicit fractional start/end timestamps and
cover the full along-track extent of their selected strip.

## Construction and optional refinement

An additional [shared-AOI grouper](JOB-GROUPING.md), enabled with
`"strips": {"grouping": {}}`, runs once before per-satellite construction. It
feeds multi-job union geometry into the same baseline strip-layout function.
The pair refinement described below remains an independent legacy option.

1. Discover relevant passes using the archived AOIs and collection-window masks.
2. For each pass/along-track look, map polygon boundaries into acquisition time
   and cross-track offset. The transform inverts the same spherical ground-offset
   model as the old builder, using the actual orbit-following frame. It preserves
   chart pieces and polygon holes. Ascending/descending are implicit in the pass
   geometry; these are not four fixed global orientations.
3. Generate two strip families anchored to opposite cross-track AOI boundaries.
   The configured `cross_spacing_km` controls overlap. Strip start/end times follow
   the AOI intersection with that band's full width. Guard margins cover numerical
   projection and boundary interpolation effects; all resulting rewards are still
   computed from the acquired polygons, not the projected construction shapes.
4. Optionally generate local paired-task proposals. Nearby requests must have
   compatible collection windows, the same along-track look and bounded combined
   width/span. Build left/right layouts for their combined projected polygons.
   Keep an added candidate only when its actual eligible crops serve both seed
   jobs (both due when `only_actions_serving_due_deadlines` is enabled).
5. Deduplicate identical physical captures, favoring the original base proposal.
   Intersect each retained capture with **all** eligible AOIs nearby, regardless
   of which AOI or pair generated it. Preserve incidental future-deadline crops.
6. Build feasible satellite paths over the resulting capture intervals.

Grouping never changes a job, deadline, priority, collection window or partial
credit rule. Sharing also exists in the base graph: clustering adds deliberately
shared placements and durations. The implementation bounds local pair proposals
rather than merging connected overlap components or enumerating all subsets.

Geometry still uses substeps at `options.step_s`, including fractional endpoints,
then unions eligible crops per job/deadline/chart within each capture. Substeps
are **not graph actions**. A job window may admit only part of a long capture;
only that part earns coverage. A full strip that exceeds the planning horizon,
configured reach or maximum duration is omitted, not silently truncated into a
claimed full acquisition. Projection and candidate enumeration are heuristics;
left/right families do not exhaust continuous pointing possibilities.

## Interval graph

Long and short alternatives may overlap at the same pointing. The new graph does
not use the old non-overlapping mode-spine assumption. It retains all feasible
nearby transitions, computed with the existing unrounded quaternion slew law.
For sufficiently distant actions it uses shared chronological waiting hubs.
The waiting threshold is a conservative maximum slew obtained through nadir, so
any later pointing is reachable. This encodes the same feasible capture sequences
as dense interval-to-interval transitions without storing all distant edges.

Action files retain the existing crop format and add construction provenance.
Schema-4 manifests carry a per-satellite pointing table. Old manifests continue
to use the global mode table. The solver supports chained waiting hubs and
validates final paths against the actual local pointing modes. It still uses the
same calibrated constant-rate maneuver surrogate, with two-second settling in
these scenarios; this change does not add a spacecraft attitude dynamics model.

## Configurations and commands

Run from the repository root:

```sh
cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-scheduling-graph -- \
  crates/leo-capture/scenarios/worldview60-aoi-strips-24h.json

cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-scheduling-graph -- \
  crates/leo-capture/scenarios/worldview60-clustered-strips-24h.json

cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-schedule-compare -- \
  crates/leo-capture/scenarios/worldview60-strips-comparison-24h.json
```

The scenarios preserve the saved WorldView-60 workload and observation model.
Outputs must be new directories. Base construction uses `"strips": {}`.
Refinement uses `"strips": {"clustering": {}}`. Omitting `strips` retains the old
builder. Clustering runs after base candidate generation and before topology
construction; a separate refined graph can be generated reproducibly from the
same preset without changing or overwriting its base graph.

Defaults for the new builder:

| Setting | Default |
| --- | ---: |
| Access discovery step | 30 seconds |
| Outer boundary guard | 50 metres |
| Start/end guard | 0.02 seconds |
| Maximum individual strip duration | 600 seconds |
| Eligible pair proposals per target | 4 |
| Maximum pair along-track gap | 3 seconds |
| Maximum pair time span | 60 seconds |
| Maximum pair width | 4 swaths |

Times are quantized outward to milliseconds and cross-track positions to one
millimetre for deterministic candidate identity. These are numerical precision
choices, not the old fixed 11.79 km pointing grid. The latter now controls mosaic
strip spacing only. The along-track alternatives remain configurable and are
−150/0/+150 km in the comparison.

Each satellite reports candidate counts, retained base/refinement actions, shared
actions, duration statistics and time spent in base proposal generation,
refinement, polygon overlay and topology construction. Summing worker timings is
CPU-worker elapsed work, not the graph's wall-clock construction time.

## Comparison and incumbent preservation

The comparison uses `aoi_strips` and `clustered_strips` as graph variants with the
same timing-policy label, epoch, observation model and preset hash. Both reward
models and all three optimizers run with the earlier search budgets. Exact
polygon acceptance and independent final verification remain enabled.

`incumbent_from: "aoi_strips"` seeds each coordinated refined run with the base
variant's result for the same optimizer/reward cell. Physical identity consists
of satellite, start/end and ground offsets; inserted actions can change numeric
IDs. Import fails if a capture is missing or the retained path is infeasible.
The independent-path baseline remains an independent solve without this seed.
Thus coordinated refined results cannot be worse than their verified retained
incumbent, while independent heuristic results can differ in either direction.
The old fixed-grid graph is a different candidate set, so its schedule is not
assumed representable by the new full-strip graphs.

## Replay and tests

Portable replay schema 2 supports arbitrary validated pointing modes and explicit
full-strip intervals. Export remaps satellite-local modes to the replay's global
table. Existing schema-1 bundles remain supported. The native visualizer draws
long acquisitions as short curved sweep pieces while keeping the scheduling action
and its job assessment intact. The bundled default visualization is unchanged.

Tests compare the compressed transition graph with dense interval feasibility,
including overlapping alternatives. The generated-graph integration case checks
incidental sharing, exact preservation of base crops, improvement from a clustered
capture, incumbent transfer, optimizer comparisons and portable replay of the
full interval. The existing capture/reward/graph/replay suites also remain active.

## Measured 24-hour results

The saved workload is unchanged: 53,000 jobs, 2,334 deadlines due in this horizon,
and preset hash `12630355598160897441`. Jobs retain their assigned priorities,
collection windows and unique-area/completed-observation reward policies. Every
selected plan is scored using polygon unions, so repeated coverage of the same
job/deadline earns no extra reward.

With priority-weighted reward `area × 2^priority`:

| Candidate graph | Independent true reward | Coordinate true reward | Lagrangian true reward | Lagrangian Met % |
| --- | ---: | ---: | ---: | ---: |
| Previous fixed grid | 1,110,875 | 1,171,328 | 1,174,949 | 53.30% |
| AOI full strips | 1,204,228 | 1,457,840 | 1,571,357 | 73.52% |
| Full strips + clustering | 1,229,558 | 1,463,973 | 1,575,783 | 73.99% |

The new base's Lagrangian result gains **33.74%** over the old graph. The refined
workflow adds **0.28%** over the new base, meeting 1,727 deadlines and missing 607
(26.01%), compared with 1,716 met and 618 missed (26.48%) for the base. Both improve
substantially over the fixed-grid result of 1,244 met and 1,090 missed (46.70%).

With area-only reward (`c = 1`), optimizing each plan for that objective:

| Candidate graph | Independent true reward | Coordinate true reward | Lagrangian true reward | Lagrangian Met % |
| --- | ---: | ---: | ---: | ---: |
| Previous fixed grid | 85,472 | 90,925 | 91,281 | 53.90% |
| AOI full strips | 92,382 | 118,586 | 121,170 | 72.84% |
| Full strips + clustering | 92,826 | 118,815 | 122,899 | 73.18% |

Here the new base gains **32.74%** and refinement adds **1.43%**. Cross-evaluation
of every selected plan under both reward models is also saved; these heuristic
searches do not guarantee that a plan optimized for one model beats every plan
optimized for another when scored under that model.

The coordinated refined runs start from the corresponding verified base result
and use another 30 Lagrangian iterations/two repair sweeps. Their gain measures
the optional refinement workflow, including continued search; it is not an
ablation isolating additional candidates from the extra search budget.

| Candidate graph | Capture candidates | Edges | Construction wall time |
| --- | ---: | ---: | ---: |
| Previous fixed grid | 141,773 | 12,030,289 | 587.2 s (archived) |
| AOI full strips | 88,728 | 2,115,511 | 31.5 s |
| Full strips + clustering | 101,743 | 3,325,698 | 47.5 s |

Base strips last 0.116–26.518 seconds, with a 1.565-second mean over all candidates.
The base already has 45,315 candidates serving multiple eligible jobs, including
incidental future-deadline work. Refinement adds 13,015 shared candidates. A full
archive comparison across all 60 satellites confirmed that all 88,728 base
captures keep identical timing, ground offsets and decoded job crops.

The new graphs were generated sequentially with eight construction workers;
optimizer runs use four workers. Fixed-grid quality and timings come from the
archived exact-polygon comparison. They are not freshly timed paired runs.
Construction and optimizer timings are single measurements, not stable hardware
benchmarks. The saved result includes both reward models, all optimizers, separate
preparation/search/verification costs, source hashes and artifact paths.

Priority-weighted optimizer costs after buffered JSON loading:

| Candidate graph | Coordinate search | Lagrangian search | Lagrangian verification |
| --- | ---: | ---: | ---: |
| Previous fixed grid (archived) | 14.2 s | 310.3 s | 31.1 s |
| AOI full strips | 1.7 s | 36.2 s | 8.1 s |
| Full strips + clustering | 1.1 s | 42.4 s | 8.9 s |

Graph/witness preparation costs 5.2/6.1 seconds for the two new graphs; their
shared polygon caches cost another 3.6/3.1 seconds. These costs are paid once per
graph in the comparison, not once per optimizer. All twelve new cells completed
in 275.7 seconds, excluding graph construction. An earlier run with unbuffered
JSON loading produced identical rewards, captures and deadline counts; it remains
archived separately and is not used for the final timing table.

Results and artifacts:

- [Portable comparison record](scenarios/worldview60-strips-comparison-result.json)
- [Comparison scenario](scenarios/worldview60-strips-comparison-24h.json)
- [Full comparison](../../artifacts/scheduling-solutions/worldview60-strips-comparison-buffered-20261116/comparison.json)
- [Best priority-weighted schedule](../../artifacts/scheduling-solutions/worldview60-strips-comparison-buffered-20261116/clustered_strips/area_priority_c2/lagrangian/schedule.json)
- [Portable replay](../../artifacts/scheduling-solutions/worldview60-strips-comparison-buffered-20261116/best-c2-replay.json.gz)
