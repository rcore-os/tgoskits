#!/usr/bin/env python3
"""Exploratory cross-source comparison against archived, never-remeasured A."""

import hashlib
import json
import statistics
from pathlib import Path

CURRENT = Path(__file__).resolve().parent
ARCHIVE = CURRENT.parent
SCENARIOS = {(policy, case) for policy in ('other', 'fifo') for case in (
    'thread_futex_same_cpu', 'thread_futex_cross_cpu', 'process_futex_cross_cpu',
    'absolute_timer_same_cpu', 'clock_pair', 'getpid', 'futex_wait_mismatch',
    'futex_wake_empty', 'sched_yield_no_peer', 'sched_yield_handoff')}
FIELDS = ('p50_ns', 'p99_ns', 'p999_ns')


def load_run(log, digest):
    content = log.read_bytes()
    assert hashlib.sha256(content).hexdigest() == digest, log
    rows = [json.loads(line.split(' ', 1)[1]) for line in content.decode().splitlines()
            if line.startswith('WAKEUP_LATENCY_RESULT ')]
    results = {(row['policy'], row['case']): row for row in rows}
    assert len(rows) == len(results) == 20 and set(results) == SCENARIOS, log
    assert all(row['samples'] == row['attempted'] and
               row['not_parked'] == row['missed_deadlines'] == 0 for row in rows), log
    return results


def archived():
    results = []
    for prefix in ('resume653', 'resume654'):
        state = json.loads((ARCHIVE / f'{prefix}-abba-status.json').read_text())
        assert state['stage'] == 'board_collection_complete' and state['board_released']
        assert state['source_head'] == '9d06d500c7ebbb960678d16d4a44aa2e036cf87e'
        for run in state['runs']:
            if run['tag'].startswith('A'):
                assert run['valid'] and run['tftp_image_sha256'] == state['images']['A']['sha256']
                results.append(load_run(ARCHIVE / run['guest_log_name'],
                                        run['guest_log_sha256']))
    assert len(results) == 4
    return results


def candidate():
    state = json.loads((CURRENT / 'resume656-abba-status.json').read_text())
    assert state['stage'] == 'board_collection_complete' and state['board_released'] is True
    assert [run['tag'] for run in state['runs']] == ['B1', 'B2']
    assert state['images']['A']['source_head'] == '9d06d500c7ebbb960678d16d4a44aa2e036cf87e'
    assert state['images']['B']['source_head'] == '62f4031a90c0d53dd36b3fd03456f14109bba5d6'
    assert state['bench']['sha256'] == '94c0a8285db8c4cae5ce3162f8a4abeead7d0e03bc8034d70e4474defba0b773'
    results = []
    for run in state['runs']:
        assert run['valid'] and run['tftp_image_sha256'] == state['images']['B']['sha256']
        results.append(load_run(CURRENT / run['guest_log_name'], run['guest_log_sha256']))
    return results


def main():
    a, b = archived(), candidate()
    rows = []
    for policy, case in sorted(SCENARIOS):
        medians = {field: {'A': statistics.median(run[policy, case][field] for run in a),
                           'B': statistics.median(run[policy, case][field] for run in b)}
                   for field in FIELDS}
        change = {field: (pair['A'] - pair['B']) / pair['A']
                  for field, pair in medians.items()}
        rows.append({'policy': policy, 'case': case, 'medians_ns': medians,
                     'improvement': change})
    regressions = [{'policy': row['policy'], 'case': row['case'], 'metric': field,
                    'improvement': row['improvement'][field]}
                   for row in rows for field in FIELDS if row['improvement'][field] <= -0.03]
    focus = next(row for row in rows if row['policy'] == 'other' and
                 row['case'] == 'thread_futex_same_cpu')
    result = {
        'scope': 'exploratory cross-source archived A vs new B, not same-source acceptance',
        'A_source': '9d06d500c7ebbb960678d16d4a44aa2e036cf87e',
        'B_source': '62f4031a90c0d53dd36b3fd03456f14109bba5d6',
        'A_boots': 4, 'B_boots': 2,
        'focus_p50_improvement': focus['improvement']['p50_ns'],
        'p50_improved_strictly_over_10pct': sum(
            row['improvement']['p50_ns'] > 0.1 for row in rows),
        'regressions_3pct_or_more': regressions,
        'exploratory_guardrails_pass': focus['improvement']['p50_ns'] > 0.1 and not regressions,
        'rows': rows,
    }
    (CURRENT / 'resume656-comparison.json').write_text(json.dumps(result, indent=2) + '\n')
    print('FOCUS_P50', round(result['focus_p50_improvement'] * 100, 2))
    print('REGRESSIONS', regressions)
    print('EXPLORATORY_PASS', result['exploratory_guardrails_pass'])


if __name__ == '__main__':
    main()
