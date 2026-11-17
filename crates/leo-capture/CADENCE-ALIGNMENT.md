# Optional geographic cadence alignment

`leo-job-align` refines an ongoing workload **before graph construction**. It
creates a new combined AOI/jobs preset; skipping this command retains randomized
deadlines. The graph builder and optimizer consume either preset normally. This
step is independent of optional strip clustering during graph construction.

Alignment changes only `acquisition.first_deadline_hours`. Each cadence retains
the exact multiset of original deadline timestamps. Consequently, the number of
deadlines in every time interval, including subsequent recurring cycles, stays
unchanged. The existing five-hour first-deadline buffer is preserved when present
in the source. AOIs, priorities, cadences, reward policies, collection spans,
solar windows, pipelines and history requirements remain unchanged.

This is a prospective service-design experiment: phase changes create alternative
job contracts. It does not silently reschedule the deadlines of an existing run,
merge jobs, manufacture historical imagery or mark missed jobs inactive.

## Two methods

### Longitude bands

Within each cadence, sort jobs by longitude and assign the sorted original
deadline times in that order. Daily deadline buckets become geographic longitude
bands, with widths determined by target density and the preserved daily counts.
Weekly jobs use seven daily buckets; 30-day monthly jobs use thirty. Populated
buckets form bands; empty original buckets remain empty.
These are load-balanced bands by job count, rather than seven equal-width slices
regardless of demand. `longitude_origin_deg` chooses the geographic wrap point.

The current saved WorldView-60 workload has mostly 13-day, 26-day and 30-day
cadences after the earlier admission redesign. It therefore uses those actual
periods, not a new seven-day requirement. The method deliberately tests the simple
band hypothesis; putting unrelated targets in one longitude band may also create
local contention.

### Balanced overlap groups

1. Use the existing equal-area polygon charts and spatial index to find actual
   AOI intersections, including holes and chart boundaries.
2. Keep pairs with the same cadence and compatible local-solar-time windows.
   A pair must overlap at least 5% of the smaller AOI by default.
3. Greedily join the strongest overlaps into groups, capped at 64 jobs and a
   100 km span of their anchor coordinates. Caps prevent a chain of overlaps
   from synchronizing an entire dense region.
4. Place groups into daily buckets while balancing total weighted area and
   regional weighted area. Regional buckets default to 5° cells. Weights use
   geometric AOI area times `priority_base^priority` (default `2`). This affects
   phase assignment, not the scheduler's separately configured objective.
5. Give group members adjacent existing deadline times within the selected day.
   Split a group only when no remaining day has enough slots. Every original
   deadline timestamp is used exactly once within its cadence.

Groups need not have identical deadline hours. Their overlapping collection
windows allow shared captures, which the ordinary graph and exact polygon reward
already support. Different cadence families are balanced separately; they cannot
generally remain synchronized indefinitely. Non-overlapping jobs remain singleton
groups. A spatial load heuristic is not a spacecraft-capacity proof.

The recommended default is overlap grouping with solar ordering, the original
daily demand target, and regional-balance weight 0.25. The initial experimental
profiles explicitly retain their earlier settings for reproducibility.
Two policy parameters address timing and demand balance:

- `within_day_order: "solar_window"` assigns each day's existing timestamps to
  groups in order of the end of their preferred solar window in UTC. Close-in-time
  daylight mosaics use 18:00 local solar time as a heuristic; actual illumination
  and opportunities are still enforced by graph construction. No window is widened.
  This avoids giving large groups early deadline hours just because the load
  allocator placed them first. `"construction"` retains that initial ordering for
  comparison. Day membership and overlap-group membership remain unchanged.
- `daily_load_target: "original_demand"` targets the source workload's weighted
  area due on each day instead of equal weighted area per job slot (`"per_job"`).
  Counts and exact timestamps remain invariant; matching weighted area is a soft
  objective. The demand-matched scenario combines this with solar ordering and a
  smaller regional-balance weight of 0.25.

## Commands

Run from the repository root:

```sh
cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-job-align -- crates/leo-capture/scenarios/worldview60-align-default.json
```

This default profile is the demand-matched, solar-aware overlap policy. Its
explicit equivalent is `worldview60-align-overlap-demand-matched.json`, whose
graph scenario is `worldview60-overlap-demand-matched-graph-24h.json`.

To reproduce the initial alternative profiles and comparison:

```sh
cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-job-align -- crates/leo-capture/scenarios/worldview60-align-longitude-bands.json
cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-job-align -- crates/leo-capture/scenarios/worldview60-align-overlap-balanced.json

cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-scheduling-graph -- crates/leo-capture/scenarios/worldview60-longitude-bands-graph-24h.json
cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-scheduling-graph -- crates/leo-capture/scenarios/worldview60-overlap-balanced-graph-24h.json

cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-schedule-compare -- crates/leo-capture/scenarios/worldview60-cadence-alignment-comparison-24h.json
```

Paths in configs resolve relative to their config file. Outputs must be new
directories. The preprocessor writes `preset.json.gz`, `assignments.json.gz` and
`report.json`, retaining the source checksum and prior assignment provenance.
It round-trips the saved preset and verifies that geometry and every other job
field are unchanged, and that each cadence's deadline multiset is identical.
Reports include daily demand, available area/reward, overlap alignment before and
after, group sizes, capacity-driven splits and construction time.

## Comparison protocol

The initial comparison uses randomized deadlines, longitude bands and balanced
overlap groups. All use the same constellation, epoch, physical model and
AOI-strip graph builder with optional strip clustering enabled. Each variant runs
independent paths, coordinate improvement and 30-iteration Lagrangian optimization
under both area-only and `c = 2` reward models, with exact polygon acceptance.
All start independently: no variant inherits another variant's schedule or search
history. Changing deadlines invalidates an old schedule as a presumed incumbent.

Daily deadline **counts** are identical, but the identities and sizes of due jobs
can change. Therefore compare true reward, met count/percentage, available reward
and the fraction of available reward earned. A higher first-day reward alone does
not prove better sustained service across the entire cadence cycle. Polygon
verification removes duplicate coverage credit and respects each job's partial
credit policy. Missing future deadlines is never inferred from missing today's
opportunity.

Tests cover seven daily bands for weekly jobs, exact deadline permutation,
order-independent assignment, grouping benefit, polygon holes, incompatible
solar windows/cadences, monthly and midnight boundaries, capacity splits,
source preservation and export round trips.

## Checking other cadence phases

The optional `scripts/evaluate-cadence-windows.py` runner samples day-offset 6 and
12 phases of the randomized, overlap-solar and demand-matched workloads:

```sh
python3 crates/leo-capture/scripts/evaluate-cadence-windows.py \
  crates/leo-capture/scenarios/worldview60-cadence-phase-validation.json
```

It rotates every job's next deadline by the same elapsed phase, modulo that job's
unchanged cadence, then rebuilds each graph. A deadline exactly on the left
boundary belongs to the preceding day. Counts remain identical between variants.
The original orbit and solar geometry are held fixed to isolate which cohort is
due; these are not propagated future calendar dates. Every snapshot starts with
empty imagery history, and rotations do not reapply the initial five-hour cutoff.
This evaluates several deadline-cycle slices, not sustained rolling execution or
a guarantee for every future day. The script records source checksums, configs,
graphs, comparisons and independently verified schedules in a new directory.

## Measured first 24-hour results

All profiles retain 53,000 jobs and exactly 2,334 deadlines due in this window.
The rows below use fresh, equal-budget Lagrangian runs with `c = 2`.

| Profile | True reward | Met deadlines | Missed deadlines | Available reward earned |
| --- | ---: | ---: | ---: | ---: |
| Randomized deadlines | 1,573,314 | 1,685 (72.19%) | 649 (27.81%) | 72.95% |
| Longitude bands | 889,920 | 751 (32.18%) | 1,583 (67.82%) | 41.96% |
| Longitude bands + solar order | 717,536 | 744 (31.88%) | 1,590 (68.12%) | 33.83% |
| Overlap groups, construction order | 1,017,935 | 1,753 (75.11%) | 581 (24.89%) | 60.35% |
| Overlap groups + solar order | 1,442,871 | 2,014 (86.29%) | 320 (13.71%) | 85.54% |
| Overlap groups + solar order + demand target | 1,625,143 | 1,981 (84.88%) | 353 (15.12%) | 83.52% |

The demand-matched profile improves priority-weighted true reward by **3.29%**
and area-only true reward by **10.60%** (from 122,864 to 135,894).
It has 1,945,713 available priority reward due, versus
2,156,649 for randomized deadlines. Its gain therefore does not come from
putting more total reward into the first day. The solar-only variant has still
less reward due (1,686,725), explaining why its higher fulfilment fraction does not
produce the largest absolute reward.

Longitude bands align overlap but also concentrate competing work geographically;
their graph has 15.77 million edges and performs poorly. The initial overlap
allocator also performs poorly in true reward because construction ordering gives
large groups early deadline hours. Solar ordering keeps their assigned days while
improving their eligibility. These initial profiles remain available as explicit
comparison settings, rather than being silently reinterpreted by new defaults.

The earlier 73.99% replay used a warm-started search. Its 1,575,783 true reward is
slightly higher than the fresh randomized result here; the aligned result also
exceeds that archived best. This experiment starts each variant independently.

| Profile | Graph actions | Graph edges | Graph construction | Lagrangian search | Final verification |
| --- | ---: | ---: | ---: | ---: | ---: |
| Randomized deadlines | 101,743 | 3,325,698 | 47.5 s | 39.7 s | 10.6 s |
| Longitude bands | 70,366 | 15,769,047 | 68.5 s | 15.8 s | 9.0 s |
| Longitude bands + solar order | 68,623 | 35,070,062 | 82.0 s | 13.6 s | 5.7 s |
| Overlap groups, construction order | 101,010 | 3,573,217 | 45.7 s | 30.5 s | 9.1 s |
| Overlap groups + solar order | 146,492 | 6,239,178 | 66.0 s | 47.2 s | 13.0 s |
| Overlap groups + solar order + demand target | 155,876 | 8,579,268 | 83.9 s | 49.3 s | 17.4 s |

Alignment itself takes about one second for 53,000 jobs; the complete preset
load/export/round-trip commands took roughly 22–24 seconds. Graph construction
uses eight workers and search uses four. Cache preparation is separate and shared
per graph in a comparison; detailed costs are in the saved reports. Timings are
single measurements and vary with machine load; the randomized graph construction
time is from its existing archived graph. No builds or tests ran concurrently
with the measured optimizer comparisons.

## Additional phase checks

The same reference orbit/solar window was also evaluated with phase offsets of
six and twelve days. These are independent cohort snapshots with empty imagery
history, not a continuous 72-hour schedule. All variants have identical due counts
within each snapshot. Each row uses `c = 2` and 30 Lagrangian iterations.

| Phase offset | Profile | True reward | Met deadlines | Missed deadlines |
| --- | --- | ---: | ---: | ---: |
| 6 days | Randomized | 1,452,082 | 1,832 (61.21%) | 1,161 (38.79%) |
| 6 days | Overlap + solar | 1,944,021 | 2,315 (77.35%) | 678 (22.65%) |
| 6 days | Overlap + solar + demand target | 1,994,880 | 2,294 (76.65%) | 699 (23.35%) |
| 12 days | Randomized | 1,768,615 | 1,843 (60.43%) | 1,207 (39.57%) |
| 12 days | Overlap + solar | 2,206,841 | 2,405 (78.85%) | 645 (21.15%) |
| 12 days | Overlap + solar + demand target | 2,155,448 | 2,414 (79.15%) | 636 (20.85%) |

Pooling the three sampled phase slices, including the original first day:

| Profile | Sum of true reward | Available reward earned | Met deadlines |
| --- | ---: | ---: | ---: |
| Randomized | 4,794,012 | 62.19% | 5,360/8,377 (63.98%) |
| Overlap + solar | 5,593,733 | 75.80% | 6,734/8,377 (80.39%) |
| Overlap + solar + demand target | 5,775,470 | 77.88% | 6,689/8,377 (79.85%) |

The recommended demand-matched profile gains **20.47%** total true reward over
randomized deadlines across these samples, with higher reward and more met
deadlines in every sampled phase. Solar-only grouping wins absolute reward in the
last slice, so this is a best aggregate result among the tested profiles, not a
universal dominance or optimality claim. Available demand still differs by area
and priority, despite exact deadline-count preservation.

The solar-aware band control also performs poorly: 717,536 true reward, 744 met
deadlines (31.88%), and 35.07 million graph edges. The selected overlap policy
therefore remains the recommended default; solar-hour ordering alone does not
rescue the concentrated-band strategy in this workload.

## Saved artifacts

- [Default alignment configuration](scenarios/worldview60-align-default.json)
- [Explicit recommended policy](scenarios/worldview60-align-overlap-demand-matched.json)
- [Recommended graph configuration](scenarios/worldview60-overlap-demand-matched-graph-24h.json)
- [Full portable comparison record](scenarios/worldview60-cadence-alignment-result.json)
- [Recommended aligned preset](../../artifacts/job-design/worldview60-aligned-default-20261116/preset.json.gz)
- [Best original-window schedule](../../artifacts/scheduling-solutions/worldview60-cadence-demand-matched-24h-20261116/overlap_demand_matched/area_priority_c2/lagrangian/schedule.json)
- [Best original-window replay](../../artifacts/scheduling-solutions/worldview60-cadence-demand-matched-24h-20261116/best-aligned-c2-replay.json.gz)

Original workloads and the bundled visualization schedule are unchanged.

Final checks passed: all six alignment tests, release builds for the five workflow
CLIs, 42 independently polygon-verified optimizer runs, and replay reconstruction
of 1,981 met / 353 missed deadlines. Running the default policy without overrides
produces a preset byte-identical to the measured recommended workload (FNV-1a64
`8615081596400989138`). The original source preset remains byte-identical to its
archived baseline copy.
