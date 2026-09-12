#!/bin/sh
set -eu

# Firefox-ESR (Gecko: SpiderMonkey + WebRender) on StarryOS, under a Weston
# (DRM backend + pixman) compositor, rendering the full https://www.4399.com/
# page with software OpenGL (llvmpipe). Same display pipeline as the NetSurf app
# (Weston DRM output -> virtio-gpu scanout -> QEMU VNC); only the browser and its
# software-GL / no-sandbox / no-dbus runtime env differ. Multi-process Gecko needs
# fork+exec, SCM_RIGHTS fd passing and memfd shm, all present in this kernel.

green="$(printf '\033[32m')"; red="$(printf '\033[31m')"; reset="$(printf '\033[0m')"
weston_pid=""; test_done=0; failed=0

fail() { printf "%sWEB_BROWSER_TEST_FAILED: %s%s\n" "$red" "$*" "$reset"; echo "WEB_BROWSER_TEST_FAILED"; failed=1; exit 1; }
cleanup() { [ -n "$weston_pid" ] && { kill "$weston_pid" 2>/dev/null || true; }; rm -f /tmp/wayland-* 2>/dev/null || true; }
on_exit() { rc=$?; cleanup; [ "$test_done" -ne 1 ] && [ "$failed" -ne 1 ] && echo "WEB_BROWSER_TEST_FAILED"; exit "$rc"; }
trap on_exit EXIT

# ---- firefox binary ----
FF=/usr/bin/firefox-esr
[ -x "$FF" ] || FF=/usr/bin/firefox
[ -x "$FF" ] || fail "firefox binary not found - prebuild may have failed"
command -v weston >/dev/null 2>&1 || fail "weston not found"
[ -e /dev/dri/card0 ] || fail "/dev/dri/card0 not found - DRM driver missing"
echo "WEB_BROWSER_PREP firefox=$FF weston + card0 present"

# ---- shared memory: Gecko IPC uses shm/memfd heavily ----
mkdir -p /dev/shm 2>/dev/null || true
mount -t tmpfs -o size=512m tmpfs /dev/shm 2>/dev/null || mount -t tmpfs tmpfs /dev/shm 2>/dev/null || true

# ---- runtime dirs / caches ----
export HOME=/root
export XDG_RUNTIME_DIR=/tmp
export XDG_CACHE_HOME=/tmp
export TMPDIR=/tmp
chmod 0700 /tmp
rm -f /tmp/wayland-* 2>/dev/null || true
export LIBSEAT_BACKEND=noop
export FONTCONFIG_PATH=/etc/fonts
mkdir -p /tmp/fontconfig /var/cache/fontconfig 2>/dev/null || true
fc-cache -f >/dev/null 2>&1 || true
glib-compile-schemas /usr/share/glib-2.0/schemas >/dev/null 2>&1 || true
# The mime.cache and icon-theme caches were baked into the rootfs at prebuild time,
# and the gdk-pixbuf loaders.cache was deliberately removed. Do NOT recreate the
# loaders.cache here: an empty one disables every loader (built-in PNG included) and
# aborts GTK icon loading. With no cache file gdk-pixbuf uses its built-in loaders.
echo "WEB_BROWSER_DIAG loaders=$(ls /usr/lib/gdk-pixbuf-2.0/2.10.0/loaders/ 2>/dev/null | wc -l) loaders_cache=$([ -f /usr/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache ] && echo present || echo absent-builtins) mime_cache=$([ -f /usr/share/mime/mime.cache ] && echo yes || echo NO)"

# ---- Weston (DRM + pixman software renderer) ----
mkdir -p /etc/xdg/weston
cat > /etc/xdg/weston/weston.ini <<'EOF'
[core]
shell=desktop-shell.so
idle-time=0
[shell]
background-color=0xff202020
locking=false
[keyboard]
keymap_layout=us
EOF
echo "WEB_BROWSER_STAGE starting weston (drm/pixman)..."
LIBGL_ALWAYS_SOFTWARE=1 /usr/bin/weston \
    --backend=drm-backend.so --renderer=pixman \
    --config=/etc/xdg/weston/weston.ini --idle-time=0 \
    --log=/tmp/weston.log >/tmp/weston-stdout.log 2>/tmp/weston-stderr.log &
weston_pid=$!

disp=""
for i in $(seq 1 120); do
    sleep 1
    kill -0 "$weston_pid" 2>/dev/null || { tail -30 /tmp/weston.log 2>/dev/null; fail "weston exited before socket"; }
    disp=$(ls /tmp/ 2>/dev/null | grep '^wayland-[0-9]*$' | head -1)
    [ -n "$disp" ] && { echo "WEB_BROWSER_STAGE wayland socket /tmp/$disp"; break; }
done
[ -n "$disp" ] || { tail -30 /tmp/weston.log 2>/dev/null; fail "no wayland socket in 120s"; }

# ---- Firefox profile + prefs (software WebRender, no sandbox, no dbus, no first-run) ----
PROFILE=/root/ffprofile
rm -rf "$PROFILE"; mkdir -p "$PROFILE"
# A fresh profile makes every run a first run, and Firefox's first-run flow
# swallows both the command-line URL and the homepage: the window comes up on
# New Tab with an empty address bar, having navigated nowhere. That failure is
# invisible from outside - it looks exactly like a page that loaded and painted
# nothing, and several runs were read the wrong way because of it. Seeding the
# two files Firefox uses to recognise an existing profile makes the run a
# subsequent one, so the address it was given is the address it opens.
printf '{"created":1700000000000,"firstUse":1700000000000}' > "$PROFILE/times.json"
: > "$PROFILE/prefs.js"

cat > "$PROFILE/user.js" <<'EOF'
user_pref("gfx.webrender.software", true);
user_pref("gfx.webrender.all", true);
user_pref("gfx.webrender.force-disabled", false);
user_pref("layers.acceleration.disabled", true);
user_pref("webgl.disabled", false);
user_pref("webgl.force-enabled", true);
user_pref("security.sandbox.content.level", 0);
user_pref("security.sandbox.gpu.level", 0);
user_pref("media.cubeb.sandbox", false);
user_pref("toolkit.telemetry.enabled", false);
user_pref("toolkit.telemetry.unified", false);
user_pref("datareporting.healthreport.uploadEnabled", false);
user_pref("datareporting.policy.dataSubmissionEnabled", false);
user_pref("app.update.enabled", false);
user_pref("browser.shell.checkDefaultBrowser", false);
user_pref("browser.startup.homepage_override.mstone", "ignore");
user_pref("browser.aboutwelcome.enabled", false);
// The load is driven through browser.startup.homepage below, and every run
// gets a fresh profile, so every run is a first run. Skipping the homepage on
// a first run therefore skipped the page under test: the window stayed on New
// Tab with an empty address bar, which is indistinguishable from a page that
// loaded and painted nothing.
user_pref("browser.startup.firstrunSkipsHomepage", false);
// open 4399 as the startup homepage: a fresh profile's first-run swallows the CLI
// URL and lands on New Tab, so drive navigation through the homepage pref instead.
user_pref("browser.startup.homepage", "https://www.4399.com/");
user_pref("browser.startup.page", 1);
user_pref("startup.homepage_welcome_url", "");
user_pref("startup.homepage_override_url", "");
user_pref("datareporting.policy.firstRunURL", "");
// Firefox dials its own push, telemetry, blocklist and captive-portal
// services on startup. Each one opens another CONNECT tunnel through the
// proxy and competes with the page for connections, which is pure noise for
// a page-render test.
user_pref("dom.push.enabled", false);
user_pref("toolkit.telemetry.enabled", false);
user_pref("datareporting.healthreport.uploadEnabled", false);
user_pref("app.update.enabled", false);
user_pref("extensions.blocklist.enabled", false);
user_pref("network.captive-portal-service.enabled", false);
user_pref("browser.safebrowsing.malware.enabled", false);
user_pref("browser.safebrowsing.phishing.enabled", false);
user_pref("browser.safebrowsing.downloads.enabled", false);
user_pref("browser.region.network.url", "");
user_pref("network.connectivity-service.enabled", false);
user_pref("dom.disable_beforeunload", true);
user_pref("network.dns.disableIPv6", true);
user_pref("dom.max_script_run_time", 0);
user_pref("dom.max_chrome_script_run_time", 0);
EOF

# Bring lo + eth0 up, then probe whether the host clash proxy is reachable through the
# SLIRP host gateway (10.0.2.2:8899). If so, route firefox through it so the REAL 4399
# (JS + gb2312 + every CDN resource) loads at host speed; otherwise fall back to
# firefox's direct network. Print the verdict so the serial log shows which path ran.
ip link set lo up 2>/dev/null || true
ip link set eth0 up 2>/dev/null || true
# A host-side relay at the SLIRP gateway (10.0.2.2:8899) forwarding to a local
# proxy makes a heavy page load at host speed, and is worth using when it is
# there. It is not part of this repository, so whether it answers is checked
# rather than assumed: pointing Firefox at a proxy that is not listening leaves
# every page blank with no other symptom, which is indistinguishable from a
# rendering failure.
relay_up=0
# Probe it the way Firefox will use it: an absolute-URI request, which only
# an HTTP proxy answers. A plain GET of / draws a 400 from any correct proxy,
# and BusyBox nc has no -z, so the previous probe reported every working
# relay as absent and sent every page down the slow direct path.
# Answering at all is not enough either: a plain file server replies 404 to
# an absolute-URI request, and pointing Firefox at something that cannot
# forward leaves every page blank - the same symptom as a failed render. A
# forward proxy relays the origin's own status, so only 1xx-3xx counts.
# Ask with wget, not nc. On this link the two disagree: the 1 MiB transfer from
# 10.0.2.2:8898 completes with wget in well under a second, while every nc probe
# of 10.0.2.2:8899 times out. The probe tool was therefore deciding the verdict
# rather than the relay's reachability, and a reachable relay got recorded as
# absent; Firefox fell back to direct connections, which this network cannot
# complete, and the load stalled with nothing in the log to say why.
if http_proxy="http://10.0.2.2:8899" wget -q -T 12 -O /dev/null http://www.4399.com/ 2>/dev/null; then
    relay_up=1
fi
echo "WEB_BROWSER_DIAG relay probe wget-via-proxy: relay_up=$relay_up"
# Keep the old answer beside it so a run states outright whether the two tools
# still disagree, rather than leaving that to be re-derived later.
if printf 'HEAD http://www.4399.com/ HTTP/1.0\r\nHost: www.4399.com\r\n\r\n' \
   | nc -w 8 10.0.2.2 8899 2>/dev/null | head -n 1 | grep -qE '^HTTP/1\.[01] [123]'; then
    echo "WEB_BROWSER_DIAG relay probe nc: reachable"
else
    echo "WEB_BROWSER_DIAG relay probe nc: unreachable"
fi

# Measure the two network paths before blaming the browser: a plain SLIRP
# transfer from the host, and a proxied fetch of the real site. A page that
# never paints looks the same whether the bytes never arrived or the renderer
# stalled, so record throughput rather than infer it.
#
# These three are diagnostics, and each one talks to a host-side service
# this repository does not ship. Under `set -e` an absent service exited
# the whole run before the first stage line, with no message and nothing
# to tell it apart from the browser failing. A measurement must not be
# able to fail the thing it measures.
t0=$(date +%s)
wget -q -T 90 -O /tmp/dl.bin http://10.0.2.2:8898/1mb.bin 2>/dev/null || true
t1=$(date +%s)
echo "WEB_BROWSER_DIAG slirp-direct 1MB bytes=$(wc -c < /tmp/dl.bin 2>/dev/null) secs=$((t1-t0))"
t0=$(date +%s)
printf 'GET http://www.4399.com/ HTTP/1.0\r\nHost: www.4399.com\r\n\r\n' | nc -w 90 10.0.2.2 8899 > /tmp/px.txt 2>/dev/null || true
t1=$(date +%s)
echo "WEB_BROWSER_DIAG proxied-4399 bytes=$(wc -c < /tmp/px.txt 2>/dev/null) secs=$((t1-t0)) status=$(head -n 1 /tmp/px.txt 2>/dev/null | tr -d '\r')"
t0=$(date +%s)
printf 'CONNECT www.4399.com:443 HTTP/1.0\r\n\r\n' | nc -w 8 10.0.2.2 8899 > /tmp/cx.txt 2>/dev/null || true
t1=$(date +%s)
echo "WEB_BROWSER_DIAG proxied-connect secs=$((t1-t0)) status=$(head -n 1 /tmp/cx.txt 2>/dev/null | tr -d '\r')"

if [ "$relay_up" = 1 ]; then
    echo "WEB_BROWSER_DIAG host relay 10.0.2.2:8899 reachable, routing through it"
    cat >> "$PROFILE/user.js" <<'PX'
user_pref("network.proxy.type", 1);
user_pref("network.proxy.http", "10.0.2.2");
user_pref("network.proxy.http_port", 8899);
user_pref("network.proxy.ssl", "10.0.2.2");
user_pref("network.proxy.ssl_port", 8899);
user_pref("network.proxy.share_proxy_settings", true);
user_pref("network.proxy.no_proxies_on", "10.0.2.2");
PX
else
    echo "WEB_BROWSER_DIAG host relay unreachable, using the guest's own network"
fi

export MOZ_ENABLE_WAYLAND=1
export GDK_BACKEND=wayland
# Firefox's WaylandProxy relays the compositor connection over an internal socket
# using an op StarryOS returns ENOTSUP for ("ProxiedConnection ... Not supported").
# Disable it so each process connects to Weston directly, like GTK/NetSurf does.
export MOZ_DISABLE_WAYLAND_PROXY=1
export WAYLAND_DISPLAY="$disp"
export LIBGL_ALWAYS_SOFTWARE=1
export GALLIUM_DRIVER=llvmpipe
export MOZ_WEBRENDER=1
export MOZ_DISABLE_CONTENT_SANDBOX=1
export MOZ_DISABLE_GMP_SANDBOX=1
export MOZ_DISABLE_RDD_SANDBOX=1
export MOZ_DISABLE_SOCKET_PROCESS_SANDBOX=1
export MOZ_SANDBOX_LOGGING=1
export DBUS_SESSION_BUS_ADDRESS=disabled:
export MOZ_CRASHREPORTER_DISABLE=1
export NO_AT_BRIDGE=1

# Gecko's network log explains a stalled load but floods the guest with I/O,
# so it is opt-in.
if [ -n "${BROWSER_HTTP_LOG:-}" ]; then
    export MOZ_LOG=timestamp,nsHttp:3,nsSocketTransport:3
    export MOZ_LOG_FILE=/tmp/ffhttp
fi
# This app renders the 4399 home page; BROWSER_URL points it elsewhere.
PAGE="${BROWSER_URL:-https://www.4399.com/}"
# A fresh profile's first run ignores the command-line URL and opens the
# homepage, so the selected page becomes the homepage too.
cat >> "$PROFILE/user.js" <<EOF
user_pref("browser.startup.homepage", "$PAGE");
EOF
# Launching is not reliable: the same command has come up navigated, come up on
# the new-tab page having gone nowhere, and not come up at all. Booting costs
# minutes and a launch costs seconds, so retry inside the one boot rather than
# spending another boot to find out. A launch counts once the address given
# actually appears in the profile, which is the same evidence the gate uses.
# Evidence that Firefox actually went to $PAGE.
#
# The history database alone cannot answer this. Firefox switches the whole
# bookmarks-and-history system off when it cannot take places.sqlite ("one of
# Firefox's files is in use by another application"), and from then on records
# nothing there however well the page loaded. That is what happens here, so the
# old check could never pass: it killed and restarted a browser that had
# navigated, parsed the document and fetched its sub-resources.
#
# Gecko writes the address it fetched into its own cache entry keys, which do
# not depend on the history system. The prefs files are deliberately not
# searched: they carry the address because this script put it there, so
# matching them would pass whatever the browser did.
navigated_to_page() {
    # Globs rather than find -exec: busybox's find does not take `+`, and the
    # disk cache follows XDG_CACHE_HOME, so it does not have to sit inside the
    # profile. An unmatched glob stays literal and is skipped by the -f test.
    for evidence in \
        "$PROFILE"/places.sqlite \
        "$PROFILE"/places.sqlite-wal \
        "$PROFILE"/sessionstore-backups/* \
        "$PROFILE"/cache2/entries/* \
        "$PROFILE"/*/cache2/entries/* \
        "${XDG_CACHE_HOME:-/tmp}"/mozilla/firefox/*/cache2/entries/* \
        /tmp/mozilla/firefox/*/cache2/entries/*
    do
        [ -f "$evidence" ] || continue
        grep -qs -- "$PAGE" "$evidence" && return 0
    done
    return 1
}

navigated_ok=0
attempt=0
while [ "$attempt" -lt 3 ]; do
    attempt=$((attempt+1))
    echo "WEB_BROWSER_STAGE launching firefox on $PAGE (attempt $attempt) ..."
    "$FF" --no-remote --new-instance --profile "$PROFILE" "$PAGE"         >/tmp/ff_stdout.log 2>/tmp/ff_err.log &
    ff_pid=$!
    k=0
    while [ "$k" -lt 12 ]; do
        sleep 10; k=$((k+1))
        if ! kill -0 "$ff_pid" 2>/dev/null; then
            ff_status=0; wait "$ff_pid" 2>/dev/null || ff_status=$?
            echo "WEB_BROWSER_DIAG launch died: status=$ff_status stderr_bytes=$(wc -c < /tmp/ff_err.log 2>/dev/null)"
            tail -n 5 /tmp/ff_err.log 2>/dev/null || true
            break
        fi
        if navigated_to_page; then
            navigated_ok=1; break
        fi
    done
    [ "$navigated_ok" = 1 ] && { echo "WEB_BROWSER_STAGE navigated on attempt $attempt"; break; }
    echo "WEB_BROWSER_DIAG cache2=$(ls -d "$PROFILE"/cache2 "$PROFILE"/*/cache2 "${XDG_CACHE_HOME:-/tmp}"/mozilla/firefox/*/cache2 /tmp/mozilla/firefox/*/cache2 2>/dev/null | tr '
' ' ') entries=$(find "$PROFILE" "${XDG_CACHE_HOME:-/tmp}"/mozilla /tmp/mozilla -path '*cache2/entries/*' -type f 2>/dev/null | wc -l)"
    echo "WEB_BROWSER_DIAG attempt $attempt did not navigate; restarting firefox"
    kill "$ff_pid" 2>/dev/null || true
    sleep 5
done
if [ "$navigated_ok" = 0 ]; then
    echo "WEB_BROWSER_DIAG no attempt navigated; holding the last one anyway"
    "$FF" --no-remote --new-instance --profile "$PROFILE" "$PAGE"         >/tmp/ff_stdout.log 2>/tmp/ff_err.log &
    ff_pid=$!
fi

# Give Gecko time to spawn content processes, fetch over TLS, run 4399's JS and
# paint via software WebRender, then hold the frame for a host-side VNC capture
# of the virtio-gpu scanout.
echo "WEB_BROWSER_RENDER_WINDOW_OPEN"
# A load that starts and then stops is the failure this test kept missing, so
# the hold watches for progress instead of only counting seconds. Gecko's cache
# gains an entry for each resource it finishes, which gives the guest a progress
# counter it can read. Once the first entry exists, a count that does not move
# for STALL_LIMIT samples means the page stopped advancing; a browser that is
# merely slow keeps adding entries.
i=0
ff_died=0
never_started=0
settled=0
last_entries=0
flat=0
SETTLE_LIMIT=20
DEAD_LIMIT=16
cache_count() { find "$PROFILE" -path '*cache2/entries/*' -type f 2>/dev/null | wc -l; }
while [ "$i" -lt 80 ]; do
    sleep 15; i=$((i+1))
    # A browser killed by a signal writes nothing to stderr, which left the log
    # showing an empty stderr and no reason at all. `wait` yields the status:
    # 128+N means signal N (137 is SIGKILL, typically the kernel reclaiming
    # memory), anything else is the browser choosing to exit.
    if ! kill -0 "$ff_pid" 2>/dev/null; then
        ff_status=0; wait "$ff_pid" 2>/dev/null || ff_status=$?
        echo "WEB_BROWSER_DIAG firefox exited early: status=$ff_status stderr_bytes=$(wc -c < /tmp/ff_err.log 2>/dev/null)"
        tail -n 5 /tmp/ff_err.log 2>/dev/null || true
        ff_died=1
        break
    fi
    n=$(cache_count)
    if [ "$n" -eq "$last_entries" ]; then
        flat=$((flat+1))
    else
        flat=0
        last_entries="$n"
    fi
    echo "WEB_BROWSER_DIAG alive t=$((i*15))s fetched=$n flat=$flat"
    # A page that has finished and a page that is stuck both stop adding cache
    # entries, so a settled count cannot mean failure on its own: read that way
    # it failed a run whose page had in fact loaded. Settling only ends the hold
    # early; whether the run passes is decided by the checks after it.
    if [ "$n" -gt 0 ] && [ "$flat" -ge "$SETTLE_LIMIT" ]; then
        echo "WEB_BROWSER_DIAG load settled at $n resources"
        settled=1
        break
    fi
    # A run that never fetched anything cannot trip the check above, because
    # that one waits for a count to stop moving and this count never started.
    # Such a run used to sit out the whole hold before the final check caught
    # it, which is twenty minutes spent learning nothing.
    if [ "$n" -eq 0 ] && [ "$i" -ge "$DEAD_LIMIT" ]; then
        echo "WEB_BROWSER_DIAG nothing fetched after $((i*15))s; the page never started loading"
        never_started=1
        break
    fi
done

echo "WEB_BROWSER_DIAG gecko log lines=$(cat /tmp/ffhttp* 2>/dev/null | wc -l)"
echo "WEB_BROWSER_DIAG === gecko: 4399 transactions ==="
cat /tmp/ffhttp* 2>/dev/null | grep -a 4399 | head -n 30
echo "WEB_BROWSER_DIAG === gecko: failures ==="
cat /tmp/ffhttp* 2>/dev/null | grep -aiE 'NS_ERROR|reset by peer|timed out|failed' | head -n 20
echo "WEB_BROWSER_DIAG === firefox stderr (head) ==="
head -40 /tmp/ff_err.log 2>/dev/null || true
# What Weston paints goes to the DRM scanout, which QEMU exports over VNC;
# /dev/fb0 is a different surface and is blank whatever happened, so the frame
# is captured from outside rather than claimed from in here.
echo "WEB_BROWSER_DIAG frame is on the VNC display; capture it there"

# What the run must show before it may call itself a pass. Surviving the hold
# is not evidence: a browser that painted nothing sat out the full twenty
# minutes and still reached this line, which is the one outcome this test
# exists to tell apart from a working render.
#
# A fetch over http leaves entries in Gecko's own cache, so a load that got
# anywhere leaves a countable artifact inside the guest. A file:// page caches
# nothing, so it is held only to the weaker check that Firefox survived.
# Navigation is not a given. A run that never left New Tab reaches this point
# looking exactly like a page that loaded and painted nothing, so check that the
# browser actually went where it was sent: Firefox records visited addresses in
# the profile, and the target appears there only if it navigated.
navigated=0
if navigated_to_page; then
    navigated=1
fi
echo "WEB_BROWSER_DIAG navigated=$navigated page=$PAGE"

cache_entries=$(find "$PROFILE" -path '*cache2/entries/*' -type f 2>/dev/null | wc -l)
echo "WEB_BROWSER_DIAG cache_entries=$cache_entries died=$ff_died"
kill "$ff_pid" 2>/dev/null || true

if [ "$ff_died" = 1 ]; then
    fail "firefox exited before the hold finished"
fi
if [ "$navigated" = 0 ]; then
    fail "firefox never navigated to $PAGE - it stayed on the new-tab page"
fi
if [ "$never_started" = 1 ]; then
    fail "$PAGE never started loading: not one resource was fetched"
fi
case "$PAGE" in
    http://*|https://*)
        if [ "$cache_entries" -lt 1 ]; then
            fail "$PAGE fetched nothing: no cache entries, so no document was loaded"
        fi
        ;;
esac

test_done=1
printf "%sWEB_BROWSER_TEST_PASSED%s\n" "$green" "$reset"
echo "WEB_BROWSER_TEST_PASSED"
exit 0
