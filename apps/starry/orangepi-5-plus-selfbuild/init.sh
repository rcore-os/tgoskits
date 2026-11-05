#!/bin/sh
# This bootstrap runs in StarryOS. All URLs refer to the active board session.
set -eu
base=/opt/tgoskits-selfbuild-app
entry=${STARRY_SELFBUILD_ENTRY_URL:?}
url=${entry%/*}
# xtask expands these from the local upload directory before sending this
# bootstrap over serial. Never obtain the expected hashes over HTTP.
run_sha256='${sessionFileSha256:usr/share/selfbuild/run.sh}'
receiver_sha256='${sessionFileSha256:usr/share/selfbuild/receive_image.py}'
manifest_sha256='${sessionFileSha256:usr/share/selfbuild/manifest.json}'
epoch_sha256='${sessionFileSha256:usr/share/selfbuild/epoch}'
mkdir -p "$base/session"
ready=1
for name in run.sh receive_image.py manifest.json epoch; do
    attempt=1
    complete=0
    while [ "$attempt" -le 30 ]; do
        if command -v curl >/dev/null 2>&1; then
            curl --connect-timeout 3 --max-time 30 -fsSL "$url/$name" -o "$base/session/$name.part" && complete=1
        elif command -v wget >/dev/null 2>&1; then
            wget -T 30 -O "$base/session/$name.part" "$url/$name" && complete=1
        fi
        if [ "$complete" = 1 ]; then
            case "$name" in
                run.sh) expected=$run_sha256 ;;
                receive_image.py) expected=$receiver_sha256 ;;
                manifest.json) expected=$manifest_sha256 ;;
                epoch) expected=$epoch_sha256 ;;
            esac
            actual=$(sha256sum "$base/session/$name.part")
            if [ "${actual%% *}" != "$expected" ]; then
                printf '===STARRY-ORANGEPI5PLUS-SELFBUILD-FAIL reason=session-checksum file=%s===\n' "$name"
                exit 1
            fi
            mv "$base/session/$name.part" "$base/session/$name"
            break
        fi
        attempt=$((attempt + 1))
        sleep 1
    done
    [ "$complete" = 1 ] || ready=0
done
if [ "$ready" = 1 ]; then
    STARRY_SELFBUILD_URL="$url" sh "$base/session/run.sh"
else
    printf '===STARRY-ORANGEPI5PLUS-SELFBUILD-%s reason=session-download===\n' FAIL
    exit 1
fi
