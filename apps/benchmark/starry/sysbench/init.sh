sh -eu <<'SYSBENCH_SCRIPT'
work=$(mktemp -d /tmp/sysbench.XXXXXX)
trap 'rm -rf "$work"' EXIT
url='${sessionFile:share/sysbench.tar.gz}'
attempt=1
while ! wget -T 30 -O "$work/bundle.part" "$url"; do
    [ "$attempt" -lt 6 ] || exit 1
    attempt=$((attempt + 1))
    sleep 5
done
mv "$work/bundle.part" "$work/bundle.tar.gz"
SYSBENCH_BUNDLE_SHA256=$(sha256sum "$work/bundle.tar.gz" | cut -d ' ' -f 1)
export SYSBENCH_BUNDLE_SHA256
mkdir "$work/tools"
tar -xzf "$work/bundle.tar.gz" -C "$work/tools"
sh "$work/tools/run.sh" board
SYSBENCH_SCRIPT
status=$?
if [ "$status" -ne 0 ]; then
    printf '\nSYSBENCH_%s\n' BOARD_FAILED
    exit "$status"
fi
