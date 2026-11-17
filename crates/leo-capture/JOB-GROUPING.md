# Shared collection AOIs before graph construction

`job_grouper::enrich(jobs, atlas, context, policy)` is the reusable entry point in
[`src/job_grouper.rs`](src/job_grouper.rs). Its rustdoc describes the full operation.
It returns the original job slice unchanged, extra collection groups, and measured
statistics. A group contains original `(job, slot)` request identities, common UTC
windows, polygon unions in the atlas's equal-area charts, and its size estimate.
Groups are geometry inputs to the existing full-strip constructor. They are never
inserted into the reward-bearing job array and do not create extra deadlines.

The optional workflow is:

```text
saved jobs (optionally deadline-aligned)
  -> shared-AOI grouper
  -> individual AOIs + group unions
  -> existing per-pass left/right full-strip construction
  -> satellite DAGs -> optimizer -> original-job polygon verification
```

## Grouping operation

1. Expand jobs into requests intersecting the planning horizon. With `due_only`,
   consider only deadlines due by the horizon end; otherwise include pending
   requests whose collection windows are already open. Different cadences can
   share a group, but different cycles of one job remain separate.
2. Compute usable intervals at each original AOI anchor using its solar-hour,
   daylight, minimum-elevation, and collection-window rules. Default sampling is
   60 seconds; solar eligibility must hold at each cell's start, middle and end.
   This is a conservative sampled eligibility estimate, not full-polygon proof.
   It can omit boundary slivers. Final crop acceptance remains authoritative.
3. Find positive polygon overlap, preserving holes and chart boundaries. Sort
   links by overlap relative to the smaller AOI, then absolute overlap and stable
   job/slot identity. The default overlap threshold is any positive area above
   numerical tolerance; `min_overlap_fraction` can restrict weak links.
4. Greedily join linked groups while intersecting the windows of **all** members.
   Pairwise-compatible windows alone cannot join a chain with no common window.
   Reject merges that exceed the configurable size or membership limits. This
   produces disjoint groups of requests; each request is in at most one group.
5. Form the actual union of each accepted group's AOIs, chart by chart, preserving
   holes. Return only groups containing two or more requests. Singleton requests
   retain their original AOI inputs.

Grouping changes no priority, deadline, cadence, collection span, reward mode, or
solar gate. It is separate from the deadline-assignment groups in `leo-job-align`.
The atlas must describe the supplied jobs in the same original index order.

## Approximate size constraint

The size policy is a candidate-generation heuristic, not a capacity reservation
or proof that the constellation can collect a complete group. Actual access,
pointing, maneuver timing and crop eligibility are checked during graph building.

At the group's mean anchor latitude, use ascending/descending ground-track
headings sampled from the WorldView-60 design orbit (cached to the nearest degree
of latitude). Project all exterior boundary vertices into a local tangent frame.
For each heading, estimate perpendicular width `W` and along-track length `L`.
Select a passing heading with the fewest estimated strips. Polygon holes reduce
union area but cannot reduce these bounding extents. Large/distant geometries
are rejected to keep the local approximation meaningful.

```text
usable_width = swath_width - 2 * boundary_margin
spacing      = min(strip_spacing, usable_width)
strips       = 1 + ceil(max(0, W - usable_width) / spacing)
capture_time = L / ground_speed

For each contiguous common time interval:
  if interval < capture_time + maneuver_allowance: accesses = 0
  otherwise: accesses = max(1, floor(interval_hours * access_fraction / access_gap_hours))

strip_budget = min(max_strips, sum(accesses) * strips_per_access)
accept if strips <= strip_budget, capture_time <= max_strip_duration,
          geographic extent <= max_span, and member_count <= max_members
```

The one-access minimum intentionally permits a short window to produce a possible
single-pass candidate. It does **not** assert a satellite is present. Disjoint
intervals cannot donate fractional accesses to each other. The default one-strip
per access assumption avoids assuming several cross-track lanes can be acquired
in one pass. Competing jobs and satellite contention are left to the scheduler.

| Parameter | Initial value | Purpose |
| --- | ---: | --- |
| Swath width | 13.1 km | Existing observation model |
| Strip spacing | 11.79 km | Existing 10% strip overlap |
| Outer boundary margin | 0.05 km per side | Same guard as baseline layouts |
| Nominal access gap | 2.4 hours | Existing workload-admission estimate |
| Strips per access | 1 | Initial parallel-strip collection assumption |
| Access fraction | 1.0 | Can be reduced to reserve slack |
| Ground speed | 7 km/s | Approximate capture duration |
| Maneuver allowance | 14 seconds | Initial 12-second repoint + 2-second settling allowance |
| Maximum strip duration | 600 seconds | Existing baseline limit |
| Maximum group span | 150 km | Bound local geometry and overlap chains |
| Maximum members | 64 | Computational cap, independent of physical size |
| Maximum strips | 8 | Additional computational cap |

For an eight-hour shared window, the nominal budget is three strips. With the
boundary guards that covers up to `13.0 + 2 * 11.79 = 36.58 km` perpendicular to
the selected heading. A four-hour window admits one strip (13.0 km); a twelve-hour
window admits five (60.16 km). Narrow overlaps between otherwise generous job
windows shrink the group's budget automatically. Real maneuvers are computed by
the graph; the 14-second number is only used by this coarse admission check.

## Standalone measurement

From the repository root:

```sh
cargo run --release --offline --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-job-group -- crates/leo-capture/scenarios/worldview60-group-jobs-aligned-24h.json

# Also measure open collection windows for jobs due beyond the first day:
cargo run --release --offline --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-job-group -- crates/leo-capture/scenarios/worldview60-group-jobs-all-windows-24h.json
```

Outputs go to a new directory; existing runs are never overwritten:

- `groups.json.gz`: groups, union polygons, chart centers, original job keys,
  source-preset checksum and resolved policy/context.
- `report.json`: group count; distinct original jobs and request memberships;
  min/max/median/mean members per group; strip/window distributions; rejected
  merge counts; and phase timings.

`statistics.elapsed_s` measures the high-level grouping function. Loading,
parallel atlas preparation and artifact export are reported separately. Empty
distributions use `null`, not fabricated zero-sized groups. Member counts exclude
singleton inputs; distinct job counts avoid counting recurring cycles twice.

## Graph integration

Use `"strips": {"grouping": {}}`, as in
[`worldview60-grouped-strips-24h.json`](scenarios/worldview60-grouped-strips-24h.json).
The graph builder calls the grouper once before launching satellite workers and
shares its results. It records `grouping-report.json` and
`collection-groups.json.gz` alongside the existing graph charts. Standalone
group exports are for inspection/reproducibility; graph generation recomputes
groups from the same inputs and policy rather than loading that export.

On each actual pass, project the union into the same acquisition-time/cross-track
coordinates as individual AOIs and reuse the existing `layouts` function. Keep
individual-AOI proposals first when deduplicating identical physical captures.
Group strips must serve at least one member's actual observation request, and
can serve different subsets of members. Every candidate is intersected with all
original nearby jobs, so incidental sharing also continues to work.

The old `"clustering": {}` pair refinement remains available separately. The new
grouped scenario uses individual + group candidates, without pair refinement.
Group counts describe additional AOI inputs, not added graph actions: a group
can produce many candidate strips across passes, or none after exact validation.

Tests cover common-window chains, different cadences, recurring slots, solar
rules, holes/chart boundaries, size budgets, deterministic membership and a real
strip-graph fixture that preserves baseline crops while adding subset-serving
group strips. Run `cargo test --offline --manifest-path crates/leo-capture/Cargo.toml --lib`.

## Measured aligned-workload results (2026-11-17)

Optimized native runs on the saved 53,000-job aligned workload, starting at
2026-11-13 00:00 UTC with a 24-hour horizon and the defaults above:

| Scope | New groups | Distinct grouped jobs | Members min–max | Median | Mean | Grouping time |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Deadlines due within 24 hours | 151 | 428 | 2–18 | 2 | 2.83 | 0.347 s |
| All open collection windows, including later deadlines | 4,080 | 16,407 | 2–64 | 2 | 4.03 | 10.477 s |

The due-only run considers 2,334 requests, of which 2,265 have sampled usable
windows. It rejects 39 proposed merges on the size constraint and none on the
64-member cap. Its groups require an estimated 1–4 strips, mean 1.73, with a mean
shared usable window of 7.82 hours.

The broader run considers 42,885 open requests; 41,140 have sampled usable
windows. There are 16,455 grouped request memberships across 16,407 distinct jobs:
48 jobs contribute different observation cycles to different groups. No request
is duplicated across groups, and no group contains two cycles of the same job.
It rejects 10,468 proposed merges on size and 1,040 on the membership cap.

Full elapsed times, including preset loading, atlas preparation and compressed
export, are **4.379 s** and **20.112 s**, respectively. Atlas preparation uses
eight workers (0.648 s / 0.671 s); grouping itself is currently sequential.

The real satellite-0 graph smoke run completed in **14.215 s**, keeping all
**3,074 baseline actions with exactly matching capture identities and crop
geometry**, and adding **261 group-generated actions**. This is a graph-generation
validation, not a measurement of optimizer reward improvement.

The capture library suite passed **102 tests**, with three existing large
benchmarks ignored. Full measurements and artifact locations are saved in
[`worldview60-job-grouping-result.json`](scenarios/worldview60-job-grouping-result.json).
