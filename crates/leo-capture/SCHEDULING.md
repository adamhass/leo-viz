# Acquisition graphs

This generates imaging choices and their transitions. New exports use schema 3
with explicit acquisition timestamps and unrounded maneuver durations. It does not select paths,
execute jobs, or certify mosaic completion. The GUI capture baseline is unchanged.

The separate [experimental scheduler](SCHEDULING-SOLVER.md) consumes these saved
graphs using Lagrangian coverage decomposition and polygon-verifies its plans.

## Run the saved workload with WorldView-60

From the repository root, after saving an AOI/jobs preset in the EO pane:

```sh
cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-scheduling-graph -- \
  crates/leo-capture/scenarios/worldview60-scheduling.json
```

The example reads `assets/presets/default-preset.json.gz` without regenerating
geometry or assignments. It uses WorldView-60: 20 planes × 3 satellites, F=14,
617 km, 97.9°, J2. The orbital epoch and planning start are both
**2026-11-13 00:00 UTC**, with a 24-hour horizon. These are design orbits, not
live spacecraft ephemerides. The output directory must not already exist.
Paths resolve relative to the config file. `first_satellite` and `satellite_count`
allow scaling tests on a contiguous subset.

Generated files are saved under `artifacts/scheduling-graphs/`, outside Cargo's
build directory and excluded from Git. The historical 30-second first run is recorded in
[`scenarios/worldview60-scheduling-result.json`](scenarios/worldview60-scheduling-result.json).
It contains 973,789 acquisition choices and 8,094,888 edges across all 60 satellites.
These are graph-generation results, not a solved observation schedule.

## Finite imaging model

- Acquisitions use a configurable sampling step, now **1 second by default**
  (0.05–30 seconds supported). Each action exports explicit `start_s` and `end_s`. The swept-scanline polygon model
  uses 2° geographic charts and 2 km polygon-edge spacing. These are configurable.
- Imaging width is 13.1 km, with constant acceptable off-nadir quality.
- Reach is a circle 450 km in diameter. Across-track offsets are 11.79 km apart;
  along-track offsets are −150, 0 and +150 km. Centres reserve half the imaging
  width inside the reach boundary. This produces **91 pointing modes**.
- Modes follow the moving ground track at fixed offsets. This is a restricted
  family of forward-scanning strips, not every possible agile imaging trajectory.
- Maneuvers use a nadir-relative quaternion surrogate. WorldView-3's published
  200 km / 12 s retargeting example is interpreted as nadir-to-target at 617 km.
  An **assumed 2-second settling allowance** gives an effective angular rate near
  1.79°/s. This is not a reconstructed spacecraft controller. Tracking the moving
  reference frame has no additional cost. No imaging occurs during maneuver edges.
- Transition feasibility is `next.start_s >= previous.end_s + slew_s`, using
  the full floating-point quaternion maneuver duration. There is no integer-step
  maneuver lag or uniform-lag hub approximation. Waiting until an available
  candidate start is recorded separately from physical slew time. Continuing in
  the same mode has no additional settling delay. Candidate imagery still uses
  the configured temporal and spatial samples; this is exact transition timing
  within that finite candidate set, not continuous trajectory optimization.
- A conservative shared one-minute solar mask limits prefilter memory independently
  of the imaging step. Exact job-window and solar/elevation checks run when
  generating crops. Satellites are still generated in parallel.
- Saved cadence, explicit next deadline (or legacy phase), and illumination rules apply. Crops crossing cadence
  boundaries are split spatially and temporally. An optional mosaic collection
  span further clips each cadence window to the interval immediately before its
  delivery deadline. Similar-solar-time mosaics retain the local solar gate;
  close-in-time mosaics retain daylight/elevation gating without that solar gate.
  Initial imagery is empty and initial pointing is nadir.
- By default, opportunities are included even without a deadline in this horizon.
  Optional `only_actions_serving_due_deadlines: true` retains only actions that
  contribute to at least one due request, preserving all shared crop records and
  original job indices. This objective-specific reduction leaves due coverage
  unchanged; it does not reward early work for future deadlines. Zero-reward
  captures are unnecessary transit maneuvers under the time-invariant metric
  slew law with free waiting. The same reduction would need reassessment for a
  non-metric controller or future-work objective.
- No storage, bandwidth, processing, energy or cloud constraints.

Reference: [WorldView-3 manufacturer datasheet](https://www.edpmedia.com/assets/maxar_brochure/WorldView3.pdf).
Finer time/pointing sampling or a different maneuver law can change feasibility.
An optimum on these graphs would apply to this finite model only.

## Sparse paths and optional acquisition

Action `a` has start node `2*a` and end node `2*a+1`, connected by two parallel
edges: **capture** earns its crop coverage; **skip** earns nothing. Both occupy
the interval in the same pointing mode. A waiting edge joins each action end
to the next action start in that mode, allowing observations to be skipped.

From an end node we need only the earliest reachable start in each other mode.
The wait chain reaches all later starts without acquiring intermediate images.
This preserves the acquisition subsequences of the dense graph for this finite
model: replace a dense transition with an earlier valid transition and wait/skip
forward. This relies on equal-length, nonoverlapping slots within a mode and the
time-invariant maneuver law. It is not a generic transitive reduction of a graph
whose vertices are mandatory observations.

New exports use direct per-mode transitions and store their unrounded durations.
The solver still reads historical schema-1/2 direct and hub encodings, deriving
legacy action timestamps from `tick * step_s`. Compaction preserves candidate
geometry and timestamps, and rewrites topology using schema 3. Merely compacting
an old 30-second candidate set cannot create the missing short acquisitions;
regenerate geometry with a finer step for those.

The sparse representation is checked against exhaustive dense feasible capture
sequences, including fractional-second capture endpoints and small versus large
slews. Selected schedules additionally receive an independent physical maneuver
check using their actual timestamps.

## Files and interpretation

Each satellite has two compressed JSONL files:

- `satellite-NNN.actions.jsonl.gz`: `id`, `tick`, `mode`, `start_s`, `end_s`, and eligible `crops`.
  Crops carry job index, cadence slot, start/end times, patch ID, area and polygons.
- `satellite-NNN.graph.jsonl.gz`: a header, action topology records, and optional
  hub records. `next_same` and `maneuver_to` reference action IDs. `maneuver_hub`
  references a graph node ID. Hub records provide `hub`, `ready_tick`, and
  `targets` (action IDs). The header supplies source/sink IDs and `source_to`.

The source represents nadir at time zero. Source transitions allow the initial
maneuver. Source-to-sink idle is always allowed; each mode's last end connects to
the sink. Unreachable actions are counted in the manifest. A solver must require
source-connected paths before crediting acquisitions.

Crop coordinates are kilometres in spherical Lambert azimuthal equal-area
charts, not longitude/latitude. `charts.json` supplies chart centres as Earth-fixed
unit vectors, with radius 6371 km and the `geometry::Chart` coordinate convention.
Job indices refer to the archived preset's `workload.jobs` order. An atlas patch
is a computational chart, not a mosaic demand cell.

One capture edge may serve several jobs. Preserve those associations and use
polygon unions for coverage/reuse; summing crop areas double-counts overlap.
The later optimizer needs coverage constraints or adaptive spatial refinement
with a polygon verifier. The union of all candidates is only an upper bound on
scheduled coverage within this candidate set, not a feasible schedule.

The manifest records configuration, constellation, source checksum, deadlines,
sizes, memory and runtime. A copy of the original preset is archived. Inputs are
never edited. Job/geometry mismatches and a preset changed during preparation
are rejected. Files use temporary paths and atomic rename. Only `manifest.json`
with `status: complete` marks a completed run; failures leave `INCOMPLETE`.

An existing complete direct graph can be compacted without recomputing crops:

```sh
crates/leo-capture/target/release/leo-scheduling-graph --compact \
  artifacts/scheduling-graphs/worldview60-24h-20261113
```

Compaction marks the manifest while working and supports retry. It does not
change candidate footprints or the archived preset.

## Resources and subsequent optimization

The example uses eight workers, bounded by the shared CPU limit. Geometry work
uses Utility QoS on macOS and 75% calculation duty. Eligibility masks are shared,
satellites are generated independently, and crop records stream directly to gzip.
The existing 16 GiB process-memory threshold applies. On macOS generation also
stops below 1 GiB free disk. These are cooperative checks, not hard OS limits.

Paths must ultimately be selected jointly across satellites: mosaics can require
several paths, while captures may serve multiple jobs. Independent per-satellite
score maximization would not solve the constellation problem. Successful 24-hour
graph generation does not imply a full-day MILP will solve efficiently. A shorter
optimization horizon carrying partial mosaics forward remains an option.

## Constellation-derived job design

`leo-job-design scenarios/worldview60-job-design.json` writes a new combined
preset and report to `artifacts/job-design/worldview60-20261113/`. The source
preset and historical scheduling artifacts remain intact. See
[JOB-DESIGN.md](JOB-DESIGN.md) for the reservation assumptions and timing modes.

After that export, `worldview60-scheduling-precise-smoke.json` builds a ten-minute
schema-3 graph for all 60 satellites at one-second imaging resolution. It is a
construction/format smoke test; the redesigned jobs have multi-day deadlines,
so this short prefix cannot assess service feasibility.

## Visualizer schedule playback

The native visualizer can replay a selected schedule on both the globe and map.
Open **Earth Observation → Schedule playback → Load schedule**. The default
path selects `artifacts/scheduling-solutions/worldview60-ongoing-baseline-24h-20261113/playback.json.gz`.
This is the independent-path candidate with 1,367 of 2,334 deadlines met (58.57%)
and 22,462 selected capture segments. The earlier priority-selected plan remains
in its original directory.
This portable file includes the archived AOIs and workload, WorldView-60 orbit
model, epoch, pointing modes, selected captures and their job/slot associations.
The large candidate-graph directory is not needed to play the exported bundle.

Create a bundle for another completed schema-3 run:

```sh
cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-schedule-playback -- PATH/TO/schedule.json PATH/TO/playback.json.gz
```

The exporter checks the graph/preset checksum, matches every selected action to
its archived record, and validates capture ordering, precise slew feasibility,
job references and collection-window bounds. It refuses to overwrite a replay.
Loading also validates embedded AOI/workload identities before replacing the view.
A failed load leaves the current scene intact. Existing schedules and presets
are not modified.

Loading runs off the UI thread, with a progress bar, elapsed time, current layer,
completed AOI/satellite counts and CPU-worker count. AOI geometry preparation and
per-satellite sweep preparation use the bounded native worker pool. Status colors
are applied before display buffers are built, avoiding a second buffer build.

The replay controls provide play/pause, restart, next capture, variable speed and
seeking through the whole horizon. Next capture pauses halfway into a sweep and
centers the globe on it. The clock uses the saved epoch and stops at the horizon.
A bright green scanline marks selected captures; plain green strips remain for
15 simulated minutes, fading with age. Observation reach and the sensor fan are
on by default, with independent checkboxes in the playback panel. A dashed cyan
450 km access corridor follows the satellite ground track even during idle periods;
it is independent of steering and is not counted as acquired imagery. The fan
follows the current interpolated sensor angle, including slew and settling. Gray
means idle, amber means slewing/settling, and green means capturing. Globe and map
views both show these cues, using the same saved orbit and epoch. They are
visual cues, not simulated satellite images. Trails are derived from the saved
actions, so large time steps and backward seeks remain correct. Replay uses wall
clock steps rather than the integrator's frame-time clamp. The hours slider only
formats its displayed value; it never rounds the running clock on idle frames.

New bundles include each due request's polygon-verified coverage and deadline.
The exporter reconstructs these from the archived selected crops and checks the
counts against the saved solver report; it does not run a new optimization.
Red AOIs have a missed deadline in this plan, including partial coverage. This
final planning status is visible throughout replay, not only after the deadline.
Teal AOIs have selected coverage and no missed deadline; gray AOIs are inactive.
Future deadlines are never marked missed. Older bundles without this assessment
still load, with a notice that per-job deadline results are unavailable.

Between captures, the pointing model waits in the previous mode, then performs
the required shortest quaternion slew and settling just before the next capture.
This is a deterministic timing convention within the feasible gap. The sensor
is off during idle/maneuver periods; no acquired trail bridges these gaps. The
fan still shows where it is pointed. The usual continuous nadir imaging swath and
capture-image previews are suppressed during replay; the cyan reach is available
access, not an imaging swath.
Leave replay restores normal AOI colors and workload/model editing.

For a native development launch with automatic loading:

```sh
LEO_VIZ_SCHEDULE=/absolute/path/to/playback.json.gz cargo run --offline --bin leo-viz
```
