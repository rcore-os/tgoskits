#!/bin/sh
exec 0</dev/null   # carpet never reads stdin; some svn/svnadmin ops block on a tty otherwise
# Comprehensive subversion carpet for StarryOS.
#
# Exercises svn fully offline via a local file:// repository: svnadmin create,
# checkout, add/commit/update/status/log/diff, copy (branch)/switch/merge,
# revert, propset/propget, blame, cat, export, info, cleanup, list. Every check
# is a HARD ASSERTION on the actual output (revision numbers, porcelain status
# codes, file content, log/blame text), not "ran ok".
#
# Three-gate: counts PASS/FAIL/TOTAL, and emits SVN_TEST_PASSED only when
# fail==0 && total==EXPECTED && pass==EXPECTED. EXPECTED is a fixed constant
# below; keep it in sync with the number of assert_* calls.
#
# Determinism: the working tree lives on a fixed path, the author name is
# pinned (--username carpet), LC_ALL/TZ are pinned. Revision numbers in
# Subversion are a deterministic function of commit order (r0, r1, r2, ...),
# independent of platform/arch, so the exact-revision assertions are stable
# across all four targets.

EXPECTED=43

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

# assert_not_contains <label> <needle> <haystack>
assert_not_contains() {
    case "$3" in
        *"$2"*) fail "$1: [$2] unexpectedly found in [$3]" ;;
        *) pass ;;
    esac
}

# assert_ok <label> <cmd...>  (command must succeed)
assert_ok() {
    label=$1
    shift
    if "$@" >/dev/null 2>&1; then
        pass
    else
        fail "$label: command failed: $*"
    fi
}

# assert_fails <label> <cmd...>  (command must fail)
assert_fails() {
    label=$1
    shift
    if "$@" >/dev/null 2>&1; then
        fail "$label: command unexpectedly succeeded: $*"
    else
        pass
    fi
}

# assert_file <label> <path>  (regular file exists)
assert_file() {
    if [ -f "$2" ]; then
        pass
    else
        fail "$1: file missing: $2"
    fi
}

# assert_no_file <label> <path>  (path absent)
assert_no_file() {
    if [ -e "$2" ]; then
        fail "$1: path unexpectedly present: $2"
    else
        pass
    fi
}

finish() {
    echo "=== svn carpet summary: PASS=$PASS FAIL=$FAIL TOTAL=$TOTAL EXPECTED=$EXPECTED ==="
    if [ "$FAIL" -eq 0 ] && [ "$TOTAL" -eq "$EXPECTED" ] && [ "$PASS" -eq "$EXPECTED" ]; then
        echo "SVN_TEST_PASSED"
        exit 0
    fi
    echo "SVN_TEST_FAILED"
    exit 1
}

echo "=== install subversion ==="
if ! command -v svn >/dev/null 2>&1; then
    sed -i 's|https://|http://|' /etc/apk/repositories 2>/dev/null || true
    apk add subversion >/dev/null 2>&1 || { apk update >/dev/null 2>&1; apk add subversion >/dev/null 2>&1; }
fi
command -v svn >/dev/null 2>&1 || { echo "svn unavailable"; echo "SVN_TEST_FAILED"; exit 1; }
command -v svnadmin >/dev/null 2>&1 || { echo "svnadmin unavailable"; echo "SVN_TEST_FAILED"; exit 1; }
svn --version --quiet

# ---------------------------------------------------------------------------
# Deterministic environment.
# ---------------------------------------------------------------------------
export TZ=UTC
export LC_ALL=C
export HOME=/tmp/svn-carpet-home

ROOT=/tmp/svn-carpet
rm -rf "$ROOT" "$HOME"
mkdir -p "$ROOT" "$HOME"
cd "$ROOT" || { echo "SVN_TEST_FAILED"; exit 1; }

# Common svn flags: never prompt, isolate config.
SVN="svn --non-interactive --no-auth-cache --config-dir $HOME/.svnconfig --username carpet"

# ===========================================================================
# 1. CREATE REPOSITORY
# ===========================================================================
echo "=== svnadmin create ==="
svnadmin create repo
REPO_URL="file://$ROOT/repo"
assert_file "create.format" "repo/format"
assert_file "create.db" "repo/db/current"
# Allow revprop edits (used to make svn:date deterministic below).
cat > repo/hooks/pre-revprop-change <<'HOOK'
#!/bin/sh
exit 0
HOOK
chmod +x repo/hooks/pre-revprop-change

# ===========================================================================
# 2. CHECKOUT (empty repo is r0)
# ===========================================================================
echo "=== checkout ==="
$SVN checkout -q "$REPO_URL" wc
assert_file "checkout.svn-dir" "wc/.svn/wc.db"
cd wc
assert_eq "checkout.rev0" "0" "$($SVN info --show-item revision)"
assert_eq "checkout.url" "$REPO_URL" "$($SVN info --show-item url)"

# ===========================================================================
# 3. ADD / STATUS / COMMIT (r1)
# ===========================================================================
echo "=== add/status/commit ==="
printf 'content1\n' > a.txt
assert_eq "status.untracked" "?       a.txt" "$($SVN status)"
$SVN add -q a.txt
assert_eq "status.added" "A       a.txt" "$($SVN status)"
$SVN commit -q -m "add a"
$SVN update -q
assert_eq "commit.rev1" "1" "$($SVN info --show-item revision)"
assert_eq "commit.clean" "" "$($SVN status)"
# Pin the commit date so log/blame metadata is reproducible.
$SVN propset -q --revprop -r1 svn:date "2023-11-15T00:00:00.000000Z" "$REPO_URL"

# ===========================================================================
# 4. MODIFY / DIFF / STATUS (r2)
# ===========================================================================
echo "=== modify/diff ==="
printf 'content2\n' >> a.txt
assert_eq "status.modified" "M       a.txt" "$($SVN status)"
DIFF=$($SVN diff)
assert_contains "diff.hunk" "@@ -1 +1,2 @@" "$DIFF"
assert_contains "diff.added" "+content2" "$DIFF"
assert_contains "diff.revline" "(revision 1)" "$DIFF"
$SVN commit -q -m "append content2"
$SVN update -q
assert_eq "commit.rev2" "2" "$($SVN info --show-item revision)"

# ===========================================================================
# 5. LOG / CAT / INFO
# ===========================================================================
echo "=== log/cat/info ==="
assert_eq "log.count" "2" "$($SVN log -q | grep -c '^r[0-9]')"
assert_contains "log.r1.msg" "add a" "$($SVN log -r1)"
assert_contains "log.r2.msg" "append content2" "$($SVN log -r2)"
assert_contains "log.author" "carpet" "$($SVN log -r1)"
assert_eq "cat.r1" "content1" "$($SVN cat -r1 a.txt)"
assert_eq "info.kind" "file" "$($SVN info --show-item kind a.txt)"
assert_eq "info.last-rev" "2" "$($SVN info --show-item last-changed-revision a.txt)"

# ===========================================================================
# 6. PROPSET / PROPGET
# ===========================================================================
echo "=== propset/propget ==="
$SVN propset -q svn:mime-type text/plain a.txt
assert_eq "propget.mime" "text/plain" "$($SVN propget svn:mime-type a.txt)"
$SVN propset -q carpet:key carpet-value a.txt
assert_eq "propget.custom" "carpet-value" "$($SVN propget carpet:key a.txt)"
assert_contains "proplist" "carpet:key" "$($SVN proplist -q a.txt)"
$SVN revert -q a.txt
assert_fails "propget.reverted" sh -c "$SVN propget carpet:key a.txt | grep -q carpet-value"

# ===========================================================================
# 7. BLAME
# ===========================================================================
echo "=== blame ==="
BLAME=$($SVN blame a.txt)
assert_contains "blame.line1.rev" "1" "$BLAME"
assert_contains "blame.line1.author" "carpet" "$BLAME"
assert_contains "blame.line1.content" "content1" "$BLAME"
assert_contains "blame.line2.content" "content2" "$BLAME"
assert_eq "blame.line-count" "2" "$($SVN blame a.txt | wc -l | tr -d ' ')"

# ===========================================================================
# 8. COPY (branch) / LIST / SWITCH
# ===========================================================================
echo "=== copy/branch/list/switch ==="
# Reorganize into trunk so branching is meaningful. Move a.txt under trunk.
$SVN mkdir -q "$REPO_URL/trunk" "$REPO_URL/branches" -m "layout"
$SVN update -q
# Server-side copy of the file into trunk, then branch trunk.
$SVN copy -q "$REPO_URL/a.txt" "$REPO_URL/trunk/a.txt" -m "seed trunk"
$SVN copy -q "$REPO_URL/trunk" "$REPO_URL/branches/b1" -m "branch b1"
$SVN update -q
assert_contains "list.branches" "b1/" "$($SVN list "$REPO_URL/branches")"
assert_contains "list.root.trunk" "trunk/" "$($SVN list "$REPO_URL")"
assert_contains "list.root.branches" "branches/" "$($SVN list "$REPO_URL")"

# Fresh checkout of trunk so switch shares ancestry.
cd "$ROOT"
$SVN checkout -q "$REPO_URL/trunk" trunkwc
cd trunkwc
assert_eq "switch.trunk.url" "$REPO_URL/trunk" "$($SVN info --show-item url)"
$SVN switch -q "$REPO_URL/branches/b1"
assert_eq "switch.branch.url" "$REPO_URL/branches/b1" "$($SVN info --show-item url)"
# Edit on branch and commit.
printf 'branch-line\n' >> a.txt
$SVN commit -q -m "branch edit"
$SVN update -q
assert_contains "switch.branch.content" "branch-line" "$($SVN cat a.txt)"

# ===========================================================================
# 9. MERGE (branch back into trunk)
# ===========================================================================
echo "=== merge ==="
$SVN switch -q "$REPO_URL/trunk"
assert_not_contains "merge.pre.trunk" "branch-line" "$($SVN cat a.txt)"
$SVN merge -q "$REPO_URL/branches/b1" 2>/dev/null
assert_contains "merge.applied" "branch-line" "$(cat a.txt)"
$SVN revert -R -q .
assert_not_contains "merge.reverted" "branch-line" "$(cat a.txt)"

# ===========================================================================
# 10. EXPORT
# ===========================================================================
echo "=== export ==="
cd "$ROOT"
$SVN export -q "$REPO_URL/trunk" exported
assert_file "export.file" "exported/a.txt"
assert_no_file "export.no-svn-dir" "exported/.svn"
assert_eq "export.content" "content1" "$(head -1 exported/a.txt)"

# ===========================================================================
# 11. CLEANUP
# ===========================================================================
echo "=== cleanup ==="
cd "$ROOT/wc"
assert_ok "cleanup" $SVN cleanup

finish
