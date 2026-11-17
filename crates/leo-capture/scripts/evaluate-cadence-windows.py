#!/usr/bin/env python3
"""Evaluate different cadence phases against the same reference 24-hour orbit window.

No historical imagery is carried between snapshots. This tests sensitivity to
deadline-cycle phase, not future orbital propagation or a rolling execution plan.
Requires already-built release binaries. All outputs go to a new directory.
"""
import argparse
import copy
import gzip
import hashlib
import json
import math
import subprocess
from pathlib import Path


def load(path):
    if path.suffix == '.gz':
        with gzip.open(path, 'rt') as stream:
            return json.load(stream)
    return json.loads(path.read_text())


def save(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


def residual(first, cadence, elapsed):
    value = (first - elapsed) % cadence
    # A deadline exactly at the snapshot start belongs to the preceding day.
    return cadence if value == 0.0 else value


def prepare(source, target, elapsed, epoch):
    envelope = load(source)
    original = copy.deepcopy(envelope['workload']['jobs'])
    for job in envelope['workload']['jobs']:
        first = job['acquisition'].get('first_deadline_hours')
        cadence = job['cadence_hours']
        if first is None or not (0 < first <= cadence):
            raise ValueError('Snapshots require explicit ongoing next deadlines')
        job['acquisition']['first_deadline_hours'] = residual(first, cadence, elapsed)
    for before, after in zip(original, envelope['workload']['jobs']):
        restored = copy.deepcopy(after)
        restored['acquisition']['first_deadline_hours'] = before['acquisition']['first_deadline_hours']
        assert restored == before
        assert 0 < after['acquisition']['first_deadline_hours'] <= after['cadence_hours']
    envelope['evaluation_window'] = {
        'source': str(source), 'source_sha256': hashlib.sha256(source.read_bytes()).hexdigest(),
        'phase_offset_hours': elapsed, 'reference_epoch_unix_s': epoch,
        'semantics': 'A common cyclic deadline-phase rotation, evaluated with the original orbit/solar window and empty imagery history. No new minimum-buffer conditioning.',
    }
    with gzip.open(target, 'wt', compresslevel=1) as stream:
        json.dump(envelope, stream, separators=(',', ':'))
    return envelope['evaluation_window']


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('config', type=Path)
    args = parser.parse_args()
    path = args.config.resolve()
    config = load(path)
    resolve = lambda value: (path.parent / value).resolve()
    output = resolve(config['output'])
    if output.exists():
        raise ValueError('Window evaluation output must be a new directory')
    template = load(resolve(config['graph_template']))
    offsets = config['phase_offsets_days']
    if len(set(offsets)) != len(offsets) or not offsets or any(not math.isfinite(x) or x < 0 for x in offsets):
        raise ValueError('Phase offsets must be distinct nonnegative day counts')
    variants = config['workloads']
    names = [v['id'] for v in variants]
    if len(names) != len(set(names)) or not names or any(not n or not all(c.isalnum() or c in '_-' for c in n) for n in names):
        raise ValueError('Invalid workload identifiers')
    capture = Path(__file__).resolve().parents[1]
    binaries = capture / 'target/release'
    for name in ['leo-scheduling-graph', 'leo-schedule-compare']:
        if not (binaries / name).is_file():
            raise ValueError(f'Build {name} in release mode first')
    output.mkdir(parents=True)
    incomplete = output / 'INCOMPLETE'
    incomplete.write_text('Window evaluation incomplete\n')
    rows, windows = [], []
    for number, offset in enumerate(offsets):
        window = output / f'window-{number:02d}'
        window.mkdir()
        # Graph sampling initializes the design fleet at t=0 for each epoch.
        # Hold the original geometry fixed instead of pretending an epoch change
        # propagates that fleet continuously into a future day.
        epoch = config['epoch_unix_s']
        entries, shifted_sources = [], []
        for variant in variants:
            directory = window / variant['id']
            directory.mkdir()
            preset = directory / 'preset.json.gz'
            shifted_sources.append(prepare(resolve(variant['preset']), preset, offset * 24, epoch))
            graph = copy.deepcopy(template)
            graph.update(preset=str(preset), output=str(directory / 'graph'), epoch_unix_s=epoch)
            graph['options']['duration_s'] = 86400
            scenario = directory / 'graph-config.json'
            save(scenario, graph)
            print(f'Day +{offset}: building {variant["id"]}', flush=True)
            with (directory / 'graph.log').open('w') as log:
                subprocess.run([str(binaries / 'leo-scheduling-graph'), str(scenario)], stdout=log, stderr=subprocess.STDOUT, check=True)
            entries.append({'id': variant['id'], 'timing_policy': variant['timing_policy'], 'graph_directory': graph['output']})
        compare = {
            'output': str(window / 'comparison'), 'workloads': entries,
            'reward_models': config['reward_models'], 'optimizers': config['optimizers'],
            'solver': config['solver'],
        }
        compare['solver']['horizon_s'] = 86400
        scenario = window / 'comparison-config.json'
        save(scenario, compare)
        print(f'Day +{offset}: comparing optimizers', flush=True)
        with (window / 'comparison.log').open('w') as log:
            subprocess.run([str(binaries / 'leo-schedule-compare'), str(scenario)], stdout=log, stderr=subprocess.STDOUT, check=True)
        report = load(window / 'comparison/comparison.json')
        assert report['status'] == 'complete'
        assert len({r['due_deadlines'] for r in report['results']}) == 1, 'Alignment changed snapshot demand counts'
        for row in report['results']:
            row['phase_offset_days'] = offset
            for field in ['schedule', 'report']:
                row[field] = str((window / 'comparison' / row[field]).relative_to(output))
            rows.append(row)
        windows.append({'offset_days': offset, 'epoch_unix_s': epoch, 'sources': shifted_sources,
                        'comparison': str((window / 'comparison/comparison.json').relative_to(output))})
        print(f'Day +{offset}: verified {len(report["results"])} plans', flush=True)
    save(output / 'result.json', {'schema_version': 1, 'status': 'complete', 'config': config,
        'semantics': 'Independent deadline-cycle phase slices against one fixed 24-hour orbit/solar window. Zero historical imagery for every variant. This is not future-date propagation or rolling execution. The first-deadline five-hour buffer applies only to the original phase; rotations are not reconditioned.',
        'release_binary_sha256': hashlib.sha256((binaries / 'leo-schedule-compare').read_bytes()).hexdigest(),
        'windows': windows, 'results': rows})
    incomplete.unlink()
    print('Saved', output / 'result.json', flush=True)


if __name__ == '__main__':
    assert residual(5, 168, 168) == 5
    assert residual(5, 168, 5) == 168
    assert residual(24, 720, 24) == 720
    main()
