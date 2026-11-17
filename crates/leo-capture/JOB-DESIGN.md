# Constellation-derived job design

For optional geographic alignment of an existing ongoing workload's deadlines,
see [cadence alignment](CADENCE-ALIGNMENT.md). It runs before graph construction
and preserves all job requirements and the original deadline counts.

`leo-job-design` adapts an existing combined AOI/jobs preset without redrawing
its polygons, priorities, pipelines, fulfilment types or output densities:

```sh
cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-job-design -- crates/leo-capture/scenarios/worldview60-job-design.json
```

The command creates a new `preset.json.gz` and `report.json`. The original preset
and previous graph/scheduler results remain untouched. The generated preset is
loadable through the application's existing preset loader. New workloads use
schema 6 (including a per-job scheduling reward policy); capture checkpoints use schema 4. Older workloads/checkpoints still
load with their historical collection semantics.

## Geometry and service admission

Each AOI is projected locally in kilometres. Ascending and descending ground-track
directions come from the WorldView-60 design orbit, including Earth rotation and
J2 propagation. Polygon-component projections give the required cross-track
intervals. A greedy interval cover counts parallel strips, skipping unoccupied
gaps between islands. The chosen direction minimizes the strip count with the
configured overlap; all strips of that product use the same direction.

The recorded `parallel_strips_lower_bound` uses the full imaging width;
`planned_strips_with_overlap` includes the configured overlap. These are bounds
and estimates in the **local parallel-strip model**, assuming one strip per pass.
They are not universal lower bounds for arbitrary agile trajectories, mixed
directions, curved tracks or a globally projected polygon. Near the orbital
turning latitude, the nearest subsatellite heading is used; real off-nadir access
and seasonal illumination still require verification.

The policy then reserves capacity across the observable corridor:

```
strip_spacing = imaging_width * (1 - overlap)
reserved_lanes = ceil(access_width / strip_spacing)  # or an explicit override
daily_pass_budget = floor(usable_window_hours / nominal_access_gap_hours)
cadence_floor_days = ceil(planned_strips * reserved_lanes / daily_pass_budget)
```

Defaults are 13.1 km imaging width, 450 km reach, 10% overlap, a nominal 2.4-hour
access gap and an eight-hour usable daily window. They yield **39 reserved lanes**
and **3 nominal usable passes/day**, hence a **13-day floor per planned strip**.
An explicit 40-lane policy gives 40/3 = 13⅓ days, rounded up to a 14-day contract.

This is a configurable **admission reservation policy**. The lane count is not a
measured set of mutually exclusive jobs, and the daily pass budget is not an
actual access forecast. It is acceptable for choosing conservative product
requirements, but it does not certify that the fleet can serve every accepted
job. Candidate generation and joint scheduling must test that separately.

Existing slower cadences are preserved. Faster cadences are raised to the policy
floor. History sample counts are adjusted to retain at least the original history
duration, with at least two samples for window pipelines. The global bounds in
the new preset expand to contain the designed jobs. Every job stores its geometric
basis and admission floor; edits below that saved floor are rejected. Changing
the underlying constellation/policy requires rerunning job design. Narrowing the
solar window below the saved admission budget also requires redesign.

Delivery phases are deterministically spread over the days of each cadence cycle
by default (`stagger_delivery_phases: true`). This avoids giving every compact
mosaic the same final collection day. The first delivery remains at
`phase + cadence`, so this is a cold-start workload with staggered first deadlines,
not a claim of preexisting imagery.

## Ongoing workloads and randomized next deadlines

`leo-job-deadlines` takes the redesigned preset and adds an explicit
`acquisition.first_deadline_hours` to every job. With seed 20261113 and a five-hour
buffer, the draw is uniform on **[5 hours, cadence)**. This is the residual
deadline distribution of an ongoing periodic job, conditioned on at least five
hours remaining. It does not clamp earlier draws to five hours or extend cadence.
A per-job hash and SplitMix64 finalizer make assignments reproducible and
independent of job ordering. All other job requirements and AOI geometry remain
unchanged; the superseded legacy phase is reset to zero.

For these jobs, slot zero ends at `first_deadline_hours`; slot `k` ends at
`first_deadline_hours + k * cadence`. Its collection window may begin before the
planning epoch. The first-day experiment credits only imagery acquired at or
after the epoch, so this is a warm start of deadline timing with empty imagery
state, not a simulation of actual historical collections. A five-hour buffer is
elapsed time, not a guarantee of two daylight/solar-window opportunities.

```sh
cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-job-deadlines -- crates/leo-capture/scenarios/worldview60-ongoing-deadlines.json
cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-scheduling-graph -- crates/leo-capture/scenarios/worldview60-ongoing-graph-24h.json
cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-schedule -- crates/leo-capture/scenarios/worldview60-ongoing-solver-24h.json
```

Each step writes a new artifact directory and refuses to overwrite previous runs.
The assignment report includes normalized deadline deciles and due-job counts by
cadence, collection mode and priority. Legacy presets without the explicit field
retain `phase + cadence` as their first deadline.

## Delivery cadence and mosaic collection are separate

`cadence_hours` determines recurring delivery deadlines. `mosaic_span_hours`
determines the maximum age of contributing imagery at each delivery deadline.
For slot `k`:

```
deadline = first_deadline + k * cadence  # ongoing
# Legacy cold start: first_deadline = phase + cadence
collection_window = [deadline - mosaic_span, deadline]
```

This implementation uses explicit windows anchored to deadlines, rather than
searching for a freely positioned collection batch. Crops crossing a window
boundary are clipped in time and space before they can earn coverage credit.
The live capture baseline, exported graph and scheduler verifier use the same
window definition. Missing a delivery deadline does not deactivate the job.

The user-selected default mix splits existing mosaic jobs as evenly as possible
(one job difference when the count is odd), deterministically ranked by AOI key,
between:

- **Similar local solar time:** daylight plus an eight-hour local solar window,
  centered at noon in the redesigned preset. Pieces may accumulate over the
  entire delivery period, so their dates can differ while their solar times match.
- **Close in elapsed time:** daylight/elevation gating without the preferred
  local-solar-time gate. The collection span is
  `ceil(planned_strips / daily_pass_budget)` whole days before delivery. This
  allows collection in a short burst while keeping a slower recurring service
  cadence. Whole-day windows avoid forcing globally distributed AOIs to acquire
  at one UTC instant. The span is a product-policy budget, not a proven shortest
  physical collection time.

**Independent crops** retain immediate partial-data usefulness and have no mosaic
coherence window. Their cadence policy budgets a full AOI refresh, but partial
data remains usable. Admission priority remains a separate numerical field;
collection modes do not replace priorities.

The UI exposes the mosaic-mode mix, per-job timing mode and collection span.
Legacy jobs with no timing fields retain their original full-cadence window and
solar rules. There is no implicit migration of saved jobs to the new policy.

## Graph timing

New schema-3 acquisition graphs store explicit start/end timestamps and direct
transition durations in seconds. They no longer round physical slew duration to
30 seconds. Imaging samples default to one second and can be reduced to 0.05 s;
spatial pointing and imagery remain discretized. The underlying quaternion law
and configurable settling allowance remain modeling assumptions.

`worldview60-scheduling-precise-smoke.json` builds ten minutes for all 60 satellites
using the redesigned preset. It tests construction and serialization, not the
multi-day service promises. Historical 24-hour exports retain their old candidates
and must be regenerated to benefit from the new imaging resolution.

## Verified design run

The saved 53,000-job workload produced 26,477 independent jobs, 13,262
similar-solar-time mosaics and 13,261 close-in-time mosaics. Cadences changed for
40,903 jobs; 50,081 jobs received a nonzero delivery phase. All original AOI
polygons, priorities, pipelines, fulfilment types and byte densities were checked
against the source preset.

| Delivery cadence | Jobs |
| --- | ---: |
| 13 days | 28,303 |
| 26 days | 7,327 |
| 30 days | 12,097 |
| 39 days | 3,836 |
| 52–273 days | 1,437 |

The ten-minute graph construction check generated all 60 satellite graphs in
18.1 seconds, with 1,656 capture actions and 96,809 edges. Independent verification
checked 91,566 direct maneuver edges and 1,824 crop intervals. Physical maneuver
durations ranged from 2.542 to 23.329 seconds; one verified transition has a
2.569-second maneuver and a 3-second interval between captures, demonstrating
that the former forced 30-second gap is gone. This cold-start prefix has no due
delivery deadlines and is not a recurring-service feasibility experiment.

The full result record is
[`scenarios/worldview60-service-design-result.json`](scenarios/worldview60-service-design-result.json).

The subsequent ongoing-workload initialization assigned all 53,000 jobs next
deadlines uniformly from five hours to their cadence (seed 20261113). It yielded
2,334 deadlines in the first 24 hours and 50,666 jobs not yet due. The observed
minimum was 5.0249 hours. Independent checks confirmed unchanged geometry and all
job requirements other than the replaced initial timing. See
[`scenarios/worldview60-ongoing-result.json`](scenarios/worldview60-ongoing-result.json)
and `SCHEDULING-SOLVER.md` for the measured first-day schedules and their
priority/search limitations.
