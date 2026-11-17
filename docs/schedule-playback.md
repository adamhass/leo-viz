# Saved observation schedules

The desktop visualizer offers a schedule dropdown. When available, the aligned
AOI-strip plan (84.88% deadlines met) is selected initially, followed in preference
by the randomized AOI-strip comparison and the bundled original baseline at
`assets/schedules/worldview60-ongoing-baseline-24h.json.gz`. Each portable bundle
includes the exact AOIs, jobs, WorldView-60 constellation, pointing modes, selected
capture intervals and polygon-verified deadline results. Loading it needs no graph
archives. The archive is read at runtime, rather than embedded in the executable.

The baseline starts at 2026-11-13 00:00 UTC and spans 24 hours. It contains 53,000
jobs, 60 satellite paths and 22,462 one-second capture segments. Of 2,334 due
deadlines, 1,367 are met (58.57%) and 967 missed (41.43%); 50,666 jobs are not yet
due. These are modeled planning outcomes, without simulated weather or execution
failures. Provenance is recorded in
[`worldview60-ongoing-baseline-result.json`](../crates/leo-capture/scenarios/worldview60-ongoing-baseline-result.json).

## Loading and inspecting

Open **Earth Observation → Schedule playback**, select a schedule, then **Load
schedule**. The picker discovers replay/playback JSON and gzip exports under
`artifacts/scheduling-solutions/`, plus the bundles in `assets/schedules/`.
**Refresh schedules** picks up new exports. Hover an entry to see its full path.
An external bundle can also be added and loaded on startup:

```sh
LEO_VIZ_SCHEDULE=/absolute/path/to/playback.json.gz cargo run --bin leo-viz
```

Loading leaves the current scene usable until the complete replacement validates.
Progress covers decoding, validation, parallel AOI preparation and per-satellite
sweep preparation. **Leave replay** restores ordinary AOI colors with the same
background loading path. Each tab keeps its own replay scene and cached geometry.

**Play schedule**, **Restart**, **Next capture**, speed and time controls operate
on saved intervals. Seeking is deterministic, including backward seeks. **Next
capture** centers the view on a scheduled sweep. AOI colors, in precedence order:

- **White:** fully captured by the current replay time for every assessed
  deadline of that job. This overrides its active/solar color and reverses on
  backward seeks. Partial coverage does not count.
- **Gray:** no deadline in the assessed scheduling horizon, even if selected
  captures incidentally cover the AOI.
- **Orange:** a missed deadline, currently outside its solar-time or daylight
  requirements, even if no captures were selected for that job.
- **Red:** at least one missed deadline in the plan, including partial coverage,
  currently eligible under its solar-time/daylight requirements.
- **Yellow:** a planned, active AOI currently outside its saved solar-time or
  daylight requirements. This updates on play, pause, restart and backward seeks.
- **Blue:** an active job with a cadence of 24 hours or less.
- **Green:** other active jobs.

New portable replay exports include `assessment.requests[].completed_at_s`,
in seconds since the replay epoch. The exporter unions the selected, clipped
crop polygons chronologically across all satellites and records the first crop
end at which the observation meets the verifier's full-coverage threshold
(relative area tolerance 1e-6). Later redundant captures do not move that time.
This is exact at the saved crop-event resolution, not a new continuous sweep
simulation. The visualizer uses this timestamp for the white cue.

Legacy bundles without timestamps use the last selected crop end for each
verified-complete observation; redundant later crops can delay their white cue.
They can be re-exported from their saved schedule and source graph without
rerunning the optimizer. A job with any missed deadline stays red/orange; an
unassessed future observation does not delay completed due work. Bundles without
assessments cannot certify completion and do not turn white. The optional field
is compatible with both existing replay schema versions.

Yellow and orange use the saved acquisition acceptance rule at the AOI's geographic anchor,
including minimum solar elevation. Close-in-time mosaics do not acquire a local
solar-time restriction, and any-time jobs stay eligible. This is a timing cue,
not a check of satellite reach, clouds, or full-polygon illumination. Gray stays
inactive. Missed deadlines return from orange to red when time-eligible; planned
daily jobs return from yellow to blue. These cues also follow backward seeks.
Colors update a small palette shared by globe/map rendering without rebuilding
polygon geometry. Older bundles
without deadline assessments load with a warning and do not invent missed jobs.

Dashed cyan shows the unsteered observation reach. The sensor fan follows the
current pointing mode: gray when idle, amber during slewing/settling, green during
capture. Slews begin at the latest feasible time before the next capture, with the
graph's angular-rate and settling model. Idle holds the last orbit-relative mode.
Green captured strips remain for 15 simulated minutes; no captured imagery is
generated. These visual layers work in both globe and map projections.

## Reproducing or replacing the baseline

Large graph archives and generated experiment outputs stay under ignored
`artifacts/` directories. The sample bundle preserves its complete designed
workload, so a fresh checkout can extract the input used by the ongoing scenarios:

```sh
python3 - <<'PY'
import gzip, json
from pathlib import Path
source = Path('assets/schedules/worldview60-ongoing-baseline-24h.json.gz')
with gzip.open(source, 'rt') as stream:
    preset = json.load(stream)['preset']
output = Path('artifacts/job-design/worldview60-ongoing-20261113/preset.json.gz')
output.parent.mkdir(parents=True, exist_ok=True)
with output.open('xb') as raw:
    with gzip.GzipFile(fileobj=raw, mode='wb', mtime=0) as stream:
        stream.write(json.dumps(preset, separators=(',', ':')).encode())
PY

cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-scheduling-graph -- crates/leo-capture/scenarios/worldview60-ongoing-graph-24h.json
cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-schedule -- --baseline crates/leo-capture/scenarios/worldview60-ongoing-baseline-24h.json
cargo run --release --manifest-path crates/leo-capture/Cargo.toml -j 2 \
  --bin leo-schedule-playback -- \
  artifacts/scheduling-solutions/worldview60-ongoing-baseline-24h-20261113/schedule.json \
  artifacts/scheduling-solutions/worldview60-ongoing-baseline-24h-20261113/playback.json.gz
```

Run from the repository root. Each output must be new; use different paths in the
scenario configurations for repeated experiments. Extraction preserves the job
data, but JSON/gzip encoding can change the input file checksum. New graphs record
their own checksum. Export requires the schedule's sibling `report.json` and the
source graph directory recorded in that report. It verifies selected actions and
deadline coverage again, then publishes the complete bundle without overwriting
an existing destination.

For new service policies, start with a combined preset saved by the app and use
[`JOB-DESIGN.md`](../crates/leo-capture/JOB-DESIGN.md). For the priority-aware solver,
omit `--baseline` and use a fresh output directory. The baseline rewards sampled
coverage independently per action and satellite; it can select redundant scans,
including forward-to-backward returns to already covered ground. Final coverage
verification unions polygons, so those repeats do not inflate met-deadline counts.
The observed misses are not a proof of fleet capacity or optimality. Improving
unique-coverage rewards is scheduler work beyond this replay snapshot.

## Maintenance map

| Area | Responsibility |
| --- | --- |
| `crates/leo-capture/src/workload.rs`, `admission.rs`, `deadlines.rs` | Shared collection windows, service policy and ongoing deadline initialization |
| `crates/leo-capture/src/scheduling/{graph,export}.rs` | Candidate DAGs, precise maneuver timing and per-satellite archives |
| `crates/leo-capture/src/scheduling/solver/` | Input indexing, path search, coverage verification and replay assessment |
| `crates/leo-capture/src/scheduling/playback.rs` | Portable schema, validation and time-dependent pointing |
| `crates/leo-capture/src/scheduling/playback/{io,export}.rs` | Bounded decoding and portable bundle export |
| `src/schedule_playback.rs` | Native replay state and clock |
| `src/schedule_playback/{catalog,loader,scene,status,panel,overlay}.rs` | Schedule discovery, background publication, prepared geometry, clock-dependent AOI status, controls and rendering |
| `src/aoi/colors.rs`, `src/aoi/gpu.rs` | Shared color palettes and persistent GPU geometry |

The scheduler owns selected intervals; replay never makes new acquisition choices.
Deadline assessment belongs in the capture library, rather than being inferred
from colors or opportunity counts in the UI. Keep geometry a pure function of the
saved plan and simulation time so accelerated playback and seeks remain reliable.

Useful checks:

```sh
cargo test --offline --manifest-path crates/leo-capture/Cargo.toml -j 2
cargo test --offline -j 2 --bin leo-viz schedule_playback
cargo test --offline -j 2 --bin leo-viz \
  saved_worldview_replay_loads_all_aois_and_only_selected_captures -- --ignored --nocapture
```

The last check loads all 53,000 AOIs and verifies the bundled counts and workload;
it is deliberately excluded from the fast test suite.
