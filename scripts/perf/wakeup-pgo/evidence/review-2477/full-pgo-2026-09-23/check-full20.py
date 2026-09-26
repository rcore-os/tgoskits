#!/usr/bin/env python3
"""Compare two complete native full20 boots with the same-source release A."""

import hashlib
import json
import statistics
from pathlib import Path


ROOT = Path(__file__).resolve().parent
BASELINE = ROOT / 'linux-rt-baseline.json'
OUTPUT = ROOT / 'resume668-comparison.json'
METRICS = ('p50_ns', 'p99_ns', 'p999_ns')


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read_runs(label, tags, state_file, baseline):
    state = json.loads(state_file.read_text())
    assert state['source_head'] == 'a1acbcfdd0377ed5e1c44cf75101bf0e8e6db891'
    assert state['board_id'] == 'OrangePi-5-Plus-1' and state['board_released']
    assert state['stage'] == 'board_collection_complete'
    assert [row['tag'] for row in state['runs']] == tags
    assert all(row['valid'] and row['samples'] == row['attempted'] == 380000
               and row['not_parked'] == row['missed_deadlines'] == 0
               for row in state['runs'])
    runs = []
    for tag, entry in zip(tags, state['runs']):
        path = state_file.parent / entry['guest_log_name']
        assert path.name == f'{label}-{tag}-full.log'
        assert sha256(path) == entry['guest_log_sha256']
        sha_file = ROOT / entry['sha256_file_name']
        assert sha256(sha_file) == entry['sha256_file_sha256']
        assert sha_file.read_text().split()[0] == baseline['bench_sha256']
        text = path.read_text()
        assert text.count('WAKEUP_LATENCY_CASE_START ') == 20
        assert text.count('WAKEUP_LATENCY_CASE_DONE ') == 20
        assert text.count('WAKEUP_LATENCY_RESULT ') == 20
        assert text.count('WAKEUP_LATENCY_PASSED') == 1
        assert 'WAKEUP_LATENCY_FAILED' not in text
        metadata = [json.loads(line.split(' ', 1)[1]) for line in text.splitlines()
                    if line.startswith('WAKEUP_LATENCY_METADATA ')]
        assert len(metadata) == 1
        assert {key: value for key, value in metadata[0].items()
                if key != 'clock_pair_min_ns'} == {key: value for key, value in
                                               baseline['metadata'][0].items()
                                               if key != 'clock_pair_min_ns'}
        assert metadata[0]['clock_pair_min_ns'] == entry['clock_pair_min_ns']
        data = [json.loads(line.split(' ', 1)[1]) for line in text.splitlines()
                if line.startswith('WAKEUP_LATENCY_RESULT ')]
        assert len(data) == 20
        assert sum(row['samples'] for row in data) == 380000
        assert all(row['samples'] == row['attempted'] and row['not_parked'] == 0
                   and row['missed_deadlines'] == 0
                   and sum(row['histogram_counts']) == row['samples'] for row in data)
        keys = {(row['policy'], row['case']) for row in data}
        assert len(keys) == 20
        for marker in ('WAKEUP_LATENCY_CASE_START ', 'WAKEUP_LATENCY_CASE_DONE '):
            parts = [dict(field.split('=', 1) for field in line.removeprefix(marker).split())
                     for line in text.splitlines() if line.startswith(marker)]
            assert {(part['policy'], part['case']) for part in parts} == keys
        runs.append({(row['policy'], row['case']): row for row in data})
    return state, runs


def main():
    baseline = json.loads(BASELINE.read_text())
    assert sha256(ROOT / 'linux-rt-baseline.raw.log') == baseline['raw_sha256']
    a_state, a = read_runs('resume666', ['A1', 'A2'], ROOT / 'resume666-a-status.json', baseline)
    b_state, b = read_runs('resume668', ['F1', 'F2'], ROOT / 'resume668-abba-status.json', baseline)
    rt = {(row['policy'], row['case']): row for row in baseline['results']}
    assert len(rt) == 20 and set(a[0]) == set(a[1]) == set(b[0]) == set(b[1]) == set(rt)
    assert a_state['bench']['sha256'] == b_state['bench']['sha256'] == baseline['bench_sha256']
    assert a_state['images']['A']['source_head'] == b_state['images']['F']['source_head']

    rows = []
    for policy, case in sorted(rt):
        key = (policy, case)
        control = {metric: statistics.median(run[key][metric] for run in a) for metric in METRICS}
        candidate = {metric: statistics.median(run[key][metric] for run in b) for metric in METRICS}
        regressions = {metric: (candidate[metric] / control[metric] - 1) * 100
                       for metric in METRICS}
        coverage = rt[key]['p50_ns'] / candidate['p50_ns'] * 100
        rows.append({'policy': policy, 'case': case, 'linux_rt_p50_ns': rt[key]['p50_ns'],
                     'control': control, 'candidate': candidate, 'rt_coverage_pct': coverage,
                     'regression_pct': regressions, 'coverage_pass': coverage > 70,
                     'regression_pass': all(value < 3 for value in regressions.values())})

    summary = {'source_head': a_state['source_head'], 'board_id': a_state['board_id'],
               'bench_sha256': a_state['bench']['sha256'], 'linux_baseline': str(BASELINE),
               'linux_baseline_sha256': sha256(BASELINE),
               'control_image_sha256': a_state['images']['A']['sha256'],
               'candidate_image_sha256': b_state['images']['F']['sha256'],
               'accepted': all(row['coverage_pass'] and row['regression_pass'] for row in rows),
               'coverage_pass_count': sum(row['coverage_pass'] for row in rows),
               'regression_failures': [row for row in rows if not row['regression_pass']],
               'worst': min(rows, key=lambda row: row['rt_coverage_pct']),
               'rows': rows}
    archived = json.loads(OUTPUT.read_text())
    assert {key: value for key, value in archived.items() if key != 'linux_baseline'} == \
           {key: value for key, value in summary.items() if key != 'linux_baseline'}, \
           'archived comparison differs from raw logs'
    assert summary['accepted'] and summary['coverage_pass_count'] == 20
    print('coverage', summary['coverage_pass_count'], '/20; regressions',
          len(summary['regression_failures']), '; accepted', summary['accepted'])
    for row in rows:
        if not (row['coverage_pass'] and row['regression_pass']):
            print(row['policy'], row['case'], 'RT_pct', round(row['rt_coverage_pct'], 2),
                  'regressions_pct', {key: round(value, 2)
                                     for key, value in row['regression_pct'].items()})


if __name__ == '__main__':
    main()
