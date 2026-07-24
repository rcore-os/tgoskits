#!/usr/bin/env bash
# Deploy the harness to the board (run from the host, board in Linux).
# Builds the instruments if missing, copies cpuprobe/membw/starry-harness.sh (and
# optionally a sysbench binary) into /usr/local/bin, then syncs (commit=600 trap).
#
#   BOARD=orangepi@169.254.50.2 SB=../../../tmp/sysbench-static/sysbench-glibc-aarch64 \
#     bash deploy-harness.sh
set -euo pipefail
cd "$(dirname "$0")"
BOARD=${BOARD:-orangepi@169.254.50.2}
PW=${BOARD_PW:-orangepi}
SB=${SB:-}   # optional: path to a board-runnable (glibc aarch64) sysbench
SSHOPTS=(-o BatchMode=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=8)
export PATH="$HOME/.orbstack/bin:$PATH"

if [ ! -x cpuprobe ] || [ ! -x membw ]; then
  echo "== building instruments =="
  bash build-harness.sh
fi

echo "== copy to $BOARD:/tmp =="
scp "${SSHOPTS[@]}" cpuprobe membw starry-harness.sh "$BOARD:/tmp/"
[ -n "$SB" ] && scp "${SSHOPTS[@]}" "$SB" "$BOARD:/tmp/sysbench.new"

echo "== install + sync on board =="
ssh "${SSHOPTS[@]}" "$BOARD" "
  set -e
  echo '$PW' | sudo -S install -m755 /tmp/cpuprobe          /usr/local/bin/cpuprobe
  echo '$PW' | sudo -S install -m755 /tmp/membw             /usr/local/bin/membw
  echo '$PW' | sudo -S install -m755 /tmp/starry-harness.sh /usr/local/bin/starry-harness.sh
  [ -f /tmp/sysbench.new ] && echo '$PW' | sudo -S install -m755 /tmp/sysbench.new /usr/bin/sysbench || true
  echo '$PW' | sudo -S sync
  echo DEPLOYED
  ls -l /usr/local/bin/cpuprobe /usr/local/bin/membw /usr/local/bin/starry-harness.sh 2>&1
  command -v sysbench && sysbench --version || echo 'sysbench: (deploy separately if missing)'
" 2>&1 | grep -v "Warning: Permanently"
