#!/usr/bin/env python3
import argparse
from pathlib import Path
import subprocess
import sys


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument('--host', default='127.0.0.1')
    parser.add_argument('--port', type=int, required=True)
    parser.add_argument('--count', type=int, default=20)
    parser.add_argument('--warmup', type=int, default=2)
    parser.add_argument('--timeout', type=float, default=1.0)
    parser.add_argument('--interval', type=float, default=0.05)
    parser.add_argument('--prefix', default='qc-reliable')
    args = parser.parse_args()

    root = Path(__file__).resolve().parents[1]
    echo_probe = root / 'scripts' / 'qc_udp_echo_probe.py'
    reliable_client = root / 'linux' / 'qc_reliable_udp_client.py'

    echo_cmd = [
        sys.executable, str(echo_probe),
        '--host', args.host,
        '--port', str(args.port),
        '--count', '5',
        '--warmup', str(args.warmup),
        '--timeout', str(args.timeout),
        '--interval', str(args.interval),
        '--prefix', args.prefix + '-echo',
    ]
    reliable_cmd = [
        sys.executable,
        str(reliable_client),
        '--host', args.host,
        '--port', str(args.port),
        '--count', str(args.count),
        '--timeout', str(args.timeout),
        '--interval', str(args.interval),
        '--retries', '3',
        '--duplicate-every', '5',
    ]

    print('phase=plain_echo command=' + ' '.join(echo_cmd), flush=True)
    echo_result = subprocess.run(echo_cmd, check=False)
    print(f'phase=plain_echo exit_code={echo_result.returncode}', flush=True)

    print('phase=reliable_udp_control command=' + ' '.join(reliable_cmd), flush=True)
    reliable_result = subprocess.run(reliable_cmd, check=False)
    print(f'phase=reliable_udp_control exit_code={reliable_result.returncode}', flush=True)

    if echo_result.returncode == 0 and reliable_result.returncode == 0:
        print('combined_result=PASS', flush=True)
        return 0

    print('combined_result=FAIL', flush=True)
    return 1


if __name__ == '__main__':
    raise SystemExit(main())
