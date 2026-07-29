#!/usr/bin/env python3
import argparse
from pathlib import Path
import subprocess
import sys


def choose_path(*candidates: Path) -> str:
    for candidate in candidates:
        if candidate.exists():
            return str(candidate)
    return str(candidates[-1])


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--host', default='127.0.0.1')
    parser.add_argument('--port', type=int, required=True)
    parser.add_argument('--count', type=int, default=20)
    parser.add_argument('--warmup', type=int, default=2)
    parser.add_argument('--timeout', type=float, default=1.0)
    parser.add_argument('--interval', type=float, default=0.05)
    parser.add_argument('--prefix', default='qc-ai-control')
    args = parser.parse_args()

    root = Path(__file__).resolve().parents[1]
    echo_probe = choose_path(root / 'scripts' / 'qc_udp_echo_probe.py',
                             Path('/tmp/qc-udp-echo-probe.py'))
    ai_demo = choose_path(
        root / 'linux' / 'qc_ai_control_demo.py',
        Path('/home/kali/qc-zephyrproject/apps/echo_server_native_zsock_20260726/tools/qc_ai_control_demo.py'),
    )

    echo_cmd = [
        sys.executable, echo_probe,
        '--host', args.host,
        '--port', str(args.port),
        '--count', '5',
        '--warmup', str(args.warmup),
        '--timeout', str(args.timeout),
        '--interval', str(args.interval),
        '--prefix', args.prefix + '-echo',
    ]
    ai_cmd = [
        sys.executable,
        ai_demo,
        '--host', args.host,
        '--port', str(args.port),
        '--count', str(args.count),
        '--timeout', str(args.timeout),
        '--interval', str(args.interval),
        '--retries', '3',
    ]

    print('phase=plain_echo command=' + ' '.join(echo_cmd), flush=True)
    echo_result = subprocess.run(echo_cmd, check=False)
    print(f'phase=plain_echo exit_code={echo_result.returncode}', flush=True)

    print('phase=ai_control command=' + ' '.join(ai_cmd), flush=True)
    ai_result = subprocess.run(ai_cmd, check=False)
    print(f'phase=ai_control exit_code={ai_result.returncode}', flush=True)

    if echo_result.returncode == 0 and ai_result.returncode == 0:
        print('combined_result=PASS', flush=True)
        return 0

    print('combined_result=FAIL', flush=True)
    return 1


if __name__ == '__main__':
    raise SystemExit(main())
