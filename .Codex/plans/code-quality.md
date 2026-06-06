# leo-viz Code Quality Plan

## Goals

- Keep the simulator behaviour stable while making the code easier to change.
- Make performance work measurable instead of guess-driven.
- Reduce the size and coupling of the largest files.

## Current Pain Points

- `src/drawing.rs`, `src/viewer.rs`, and `src/app.rs` contain most of the program logic and mix UI, draw prep, interaction, simulation state, and presentation-specific behaviour.
- Several public functions take very large parameter lists, especially `draw_3d_view`.
- Shared state is often passed as tuples, which makes call sites hard to audit.
- Hot paths allocate new vectors every frame for satellite positions, TLE propagation, shell filtering, and draw data.
- `SatelliteState` combines hot numeric data with cold metadata such as names and optional TLE fields.

## Near-Term Refactors

1. Split drawing by view.
   - Move globe code to `drawing/globe.rs`.
   - Move map code to `drawing/map.rs`.
   - Move torus code to `drawing/torus.rs`.
   - Move routing/path helpers to `drawing/routing.rs`.
   - Move labels and hover helpers to `drawing/labels.rs`.

2. Introduce draw input/state structs.
   - Replace the long `draw_3d_view` parameter list with `Draw3dInput`, `Draw3dState`, and `Draw3dOutput`.
   - Do the same for map and torus once the split is stable.

3. Name shared data shapes.
   - Replace repeated tuple types with local aliases or small structs.
   - Prefer structs when fields have meaning beyond positional grouping.

4. Add focused benchmarks.
   - `WalkerConstellation::satellite_positions`.
   - SGP4 propagation for Starlink-scale TLE input.
   - TLE shell filtering.
   - `compute_knn_neighbor_lists`.
   - SpaceCoMP job assignment and path computation.

5. Reduce per-frame allocation.
   - Add `satellite_positions_into(&mut Vec<SatelliteState>, time)`.
   - Reuse TLE propagation buffers per preset/shell.
   - Avoid rebuilding shell `HashSet`s each frame.
   - Cache stable Walker ISL topology when topology parameters have not changed.

## Longer-Term Refactors

- Split `SatelliteState` into hot and cold data.
  - Hot: position, lat/lon, ascending, plane, satellite index.
  - Cold: label, TLE metadata, display metadata.
- Consider `SmallVec<[usize; 8]>` or fixed arrays for ISL neighbours.
- Consider a struct-of-arrays layout only after benchmarks show satellite iteration is a bottleneck.
- Move presentation-specific decisions out of generic viewer update code.

## Guardrails

- Keep changes incremental and benchmarkable.
- Avoid visual regressions during refactors.
- Run `cargo check`, `cargo test`, and at least one local presentation smoke test after changes touching draw code.
