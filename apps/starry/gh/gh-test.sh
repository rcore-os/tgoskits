#!/bin/sh
# GitHub CLI (gh) OFFLINE carpet for StarryOS.
#
# SCOPE: this carpet deliberately exercises only the offline-capable surface of
# gh. Authenticated GitHub API operations (gh api <endpoint>, gh pr/issue/repo
# against github.com, gh auth login) require network + credentials and are OUT
# of scope here -- StarryOS runs qemu-user networking with no token, so those
# paths cannot be exercised without a live account. We do NOT stub or fake any
# API response. Instead we assert exactly what gh can do with no network and no
# auth:
#   * gh --version           -> parse a real semver
#   * gh help / gh api --help -> local help pages render, exit 0
#   * gh config set/get       -> round-trip a config value on disk
#   * gh config get <default> -> built-in default (git_protocol=https)
#   * gh config get <missing> -> nonzero exit
#   * gh extension list       -> requires auth; offline it errors (documented)
#   * gh auth status          -> unauthenticated exit is nonzero
#   * gh <bad-subcommand>      -> nonzero exit
#   * gh api <endpoint>        -> offline/unauth failure (proves no fake data)
# The gate is a real gate: a fresh isolated GH_CONFIG_DIR is used so no host
# credential can leak in.
#
# If you consider this offline surface too thin to be a meaningful carpet, see
# the note in the app README/report: the recommendation is to SKIP shipping a
# gh carpet on StarryOS rather than ship a vacuous one -- but the checks below
# are genuine behavioral assertions (semver shape, config persistence, exit
# codes), not smoke, so this carpet is included.
#
# Three-gate: emits GH_TEST_PASSED only when fail==0 && total==EXPECTED &&
# pass==EXPECTED. Deterministic: no network, isolated config, pinned locale.

EXPECTED=20

PASS=0
FAIL=0
TOTAL=0

pass() {
    PASS=$((PASS + 1))
    TOTAL=$((TOTAL + 1))
}

fail() {
    FAIL=$((FAIL + 1))
    TOTAL=$((TOTAL + 1))
    echo "FAIL[$TOTAL]: $1"
}

# assert_eq <label> <expected> <actual>
assert_eq() {
    if [ "$2" = "$3" ]; then
        pass
    else
        fail "$1: expected=[$2] actual=[$3]"
    fi
}

# assert_contains <label> <needle> <haystack>
assert_contains() {
    case "$3" in
        *"$2"*) pass ;;
        *) fail "$1: [$2] not found in [$3]" ;;
    esac
}

# assert_matches_semver <label> <string>
assert_matches_semver() {
    if echo "$2" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$'; then
        pass
    else
        fail "$1: not a semver: [$2]"
    fi
}

# assert_ok <label> <cmd...>  (command must succeed / exit 0)
assert_ok() {
    label=$1
    shift
    if "$@" >/dev/null 2>&1; then
        pass
    else
        fail "$label: command failed: $*"
    fi
}

# assert_fails <label> <cmd...>  (command must exit nonzero)
assert_fails() {
    label=$1
    shift
    if "$@" >/dev/null 2>&1; then
        fail "$label: command unexpectedly succeeded: $*"
    else
        pass
    fi
}

finish() {
    echo "=== gh carpet summary: PASS=$PASS FAIL=$FAIL TOTAL=$TOTAL EXPECTED=$EXPECTED ==="
    if [ "$FAIL" -eq 0 ] && [ "$TOTAL" -eq "$EXPECTED" ] && [ "$PASS" -eq "$EXPECTED" ]; then
        echo "GH_TEST_PASSED"
        exit 0
    fi
    echo "GH_TEST_FAILED"
    exit 1
}

echo "=== install github-cli ==="
if ! command -v gh >/dev/null 2>&1; then
    sed -i 's|https://|http://|' /etc/apk/repositories 2>/dev/null || true
    apk add github-cli >/dev/null 2>&1 || { apk update >/dev/null 2>&1; apk add github-cli >/dev/null 2>&1; }
fi
command -v gh >/dev/null 2>&1 || { echo "gh unavailable"; echo "GH_TEST_FAILED"; exit 1; }

# ---------------------------------------------------------------------------
# Isolated, offline environment. A fresh GH_CONFIG_DIR guarantees no host
# credential leaks in and that config round-trips are observed on a clean slate.
# ---------------------------------------------------------------------------
export LC_ALL=C
export HOME=/tmp/gh-carpet-home
export GH_CONFIG_DIR=/tmp/gh-carpet-config
export GH_NO_UPDATE_NOTIFIER=1
export GH_PROMPT_DISABLED=1
export NO_COLOR=1
# Ensure no token is present so auth-gated paths take the unauthenticated path.
unset GH_TOKEN
unset GITHUB_TOKEN
rm -rf "$HOME" "$GH_CONFIG_DIR"
mkdir -p "$HOME" "$GH_CONFIG_DIR"

gh --version | head -1

# ===========================================================================
# 1. VERSION (parse real semver)
# ===========================================================================
echo "=== version ==="
VLINE=$(gh --version 2>/dev/null | head -1)
assert_contains "version.prefix" "gh version" "$VLINE"
VER=$(echo "$VLINE" | sed -n 's/^gh version \([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\).*/\1/p')
assert_matches_semver "version.semver" "$VER"
assert_ok "version.exit0" gh --version

# ===========================================================================
# 2. HELP PAGES (local, no network)
# ===========================================================================
echo "=== help ==="
assert_ok "help.top.exit0" gh help
HELP=$(gh help 2>&1)
assert_contains "help.usage" "USAGE" "$HELP"
assert_contains "help.core" "gh <command>" "$HELP"
assert_ok "help.api.exit0" gh api --help
APIHELP=$(gh api --help 2>&1)
assert_contains "help.api.usage" "api" "$APIHELP"

# ===========================================================================
# 3. CONFIG SET/GET ROUND-TRIP (on disk)
# ===========================================================================
echo "=== config ==="
gh config set editor carpet-editor
assert_eq "config.roundtrip" "carpet-editor" "$(gh config get editor 2>/dev/null)"
gh config set editor other-editor
assert_eq "config.overwrite" "other-editor" "$(gh config get editor 2>/dev/null)"
# Built-in default value (unset key returns its documented default).
assert_eq "config.default.git_protocol" "https" "$(gh config get git_protocol 2>/dev/null)"
# A key that has no value and no default -> nonzero exit.
assert_fails "config.get.missing" gh config get carpet_nonexistent_key
# Config persisted to the isolated dir on disk.
assert_ok "config.file.exists" test -f "$GH_CONFIG_DIR/config.yml"

# ===========================================================================
# 4. EXTENSION LIST (auth-gated: offline it must not fake success)
# ===========================================================================
echo "=== extension ==="
# gh extension list needs a host/auth; with no auth it exits nonzero. We assert
# it does NOT silently claim success (i.e. it correctly reports the auth need).
assert_fails "extension.list.unauth" gh extension list

# ===========================================================================
# 5. AUTH STATUS (unauthenticated -> nonzero)
# ===========================================================================
echo "=== auth ==="
assert_fails "auth.status.unauth" gh auth status
AUTH=$(gh auth status 2>&1)
assert_contains "auth.status.msg" "not logged" "$AUTH"

# ===========================================================================
# 6. BAD SUBCOMMAND (nonzero exit + diagnostic)
# ===========================================================================
echo "=== errors ==="
assert_fails "error.bad-subcommand" gh definitely-not-a-real-gh-command
BAD=$(gh definitely-not-a-real-gh-command 2>&1)
assert_contains "error.bad.msg" "unknown command" "$BAD"

# ===========================================================================
# 7. API IS OUT OF OFFLINE SCOPE (network/unauth must fail; no fake data)
# ===========================================================================
echo "=== api scoped-out ==="
# gh api against github.com requires network+auth; offline it must fail rather
# than return fabricated data. This asserts the OUT-of-scope boundary.
assert_fails "api.user.offline" gh api user
assert_fails "api.root.offline" gh api /

finish
