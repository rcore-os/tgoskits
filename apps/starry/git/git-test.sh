#!/bin/sh
# Comprehensive git carpet for StarryOS.
#
# Exercises git across plumbing, porcelain, branching/merging, history rewrite,
# stash/tag, transport (local/bare/bundle/archive), worktree/submodule/
# sparse-checkout, and maintenance (reflog/blame/bisect/rerere/gc/fsck/notes/
# format-patch+am). Every check is a HARD ASSERTION on the actual output
# (exact SHA / porcelain / content / revision), not "ran ok".
#
# Three-gate: counts PASS/FAIL/TOTAL, and emits GIT_TEST_PASSED only when
# fail==0 && total==EXPECTED && pass==EXPECTED. EXPECTED is a fixed constant
# below; keep it in sync with the number of assert_* calls.
#
# Determinism: author/committer identity, dates and TZ are pinned so that the
# object hashes asserted below are reproducible. Git object SHAs are a function
# of content+identity+date only (no platform/arch input), so the exact-SHA
# assertions are stable across all four targets.

EXPECTED=138

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
    echo "=== git carpet summary: PASS=$PASS FAIL=$FAIL TOTAL=$TOTAL EXPECTED=$EXPECTED ==="
    if [ "$FAIL" -eq 0 ] && [ "$TOTAL" -eq "$EXPECTED" ] && [ "$PASS" -eq "$EXPECTED" ]; then
        echo "GIT_TEST_PASSED"
        exit 0
    fi
    echo "GIT_TEST_FAILED"
    exit 1
}

echo "=== install git ==="
if ! command -v git >/dev/null 2>&1; then
    sed -i 's|https://|http://|' /etc/apk/repositories 2>/dev/null || true
    apk add git >/dev/null 2>&1 || { apk update >/dev/null 2>&1; apk add git >/dev/null 2>&1; }
fi
command -v git >/dev/null 2>&1 || { echo "git unavailable"; echo "GIT_TEST_FAILED"; exit 1; }
git --version

# ---------------------------------------------------------------------------
# Deterministic environment: fixed identity + fixed dates make object hashes
# reproducible so we can pin exact SHAs.
# ---------------------------------------------------------------------------
export GIT_AUTHOR_NAME="Carpet Tester"
export GIT_AUTHOR_EMAIL="carpet@starry.os"
export GIT_COMMITTER_NAME="Carpet Tester"
export GIT_COMMITTER_EMAIL="carpet@starry.os"
export GIT_AUTHOR_DATE="1700000000 +0000"
export GIT_COMMITTER_DATE="1700000000 +0000"
export TZ=UTC
export LC_ALL=C
export GIT_PAGER=cat
export PAGER=cat
export GIT_TERMINAL_PROMPT=0
export GIT_CONFIG_NOSYSTEM=1
export HOME=/tmp/git-carpet-home
export GIT_CONFIG_GLOBAL=/tmp/git-carpet-home/.gitconfig

ROOT=/tmp/git-carpet
rm -rf "$ROOT" "$HOME"
mkdir -p "$ROOT" "$HOME"
cd "$ROOT" || { echo "GIT_TEST_FAILED"; exit 1; }

git config --global init.defaultBranch master
git config --global user.name "Carpet Tester"
git config --global user.email "carpet@starry.os"
git config --global advice.detachedHead false
git config --global protocol.file.allow always
git config --global gc.auto 0
git config --global core.autocrlf false

# ===========================================================================
# 1. PLUMBING
# ===========================================================================
echo "=== plumbing ==="
mkdir plumb && cd plumb
git init -q

assert_file "plumb.git.HEAD" ".git/HEAD"
assert_eq "plumb.symbolic-ref" "refs/heads/master" "$(git symbolic-ref HEAD)"

# "hello\n" -> ce013625030ba8dba906f756967f9e9ca394464a (SHA-1 blob invariant)
printf 'hello\n' > hello.txt
BLOB=$(git hash-object hello.txt)
assert_eq "plumb.hash-object.blob" "ce013625030ba8dba906f756967f9e9ca394464a" "$BLOB"

BLOB2=$(git hash-object -w hello.txt)
assert_eq "plumb.hash-object.-w" "$BLOB" "$BLOB2"
assert_eq "plumb.cat-file.-t" "blob" "$(git cat-file -t "$BLOB")"
assert_eq "plumb.cat-file.-s" "6" "$(git cat-file -s "$BLOB")"
assert_eq "plumb.cat-file.-p" "hello" "$(git cat-file -p "$BLOB")"

git update-index --add --cacheinfo 100644 "$BLOB" hello.txt
TREE=$(git write-tree)
assert_eq "plumb.write-tree" "aaa96ced2d9a1c8e72c56b253a0e2fe78393feb7" "$TREE"
assert_eq "plumb.ls-files.stage" "100644 $BLOB 0	hello.txt" "$(git ls-files -s)"
assert_eq "plumb.ls-tree" "100644 blob $BLOB	hello.txt" "$(git ls-tree "$TREE")"
assert_eq "plumb.ls-tree.name-only" "hello.txt" "$(git ls-tree --name-only "$TREE")"

CMT=$(echo "root" | git commit-tree "$TREE")
assert_eq "plumb.commit-tree.sha" "d447bb52cc7f940f60cfe7a66d4d91e5740cc9c1" "$CMT"
assert_eq "plumb.cat-file.commit.type" "commit" "$(git cat-file -t "$CMT")"
assert_contains "plumb.commit.tree-line" "tree $TREE" "$(git cat-file -p "$CMT")"

TREE2=$(git ls-tree "$TREE" | git mktree)
assert_eq "plumb.mktree.roundtrip" "$TREE" "$TREE2"

git update-ref refs/heads/master "$CMT"
git symbolic-ref HEAD refs/heads/master
assert_eq "plumb.rev-parse.HEAD" "$CMT" "$(git rev-parse HEAD)"
assert_eq "plumb.rev-parse.short" "d447bb5" "$(git rev-parse --short=7 HEAD)"
assert_eq "plumb.rev-parse.abbrev-ref" "master" "$(git rev-parse --abbrev-ref HEAD)"

cd "$ROOT"

# ===========================================================================
# 2. BASICS
# ===========================================================================
echo "=== basics ==="
mkdir basic && cd basic
git init -q

printf 'line1\n' > a.txt
assert_eq "basic.status.untracked" "?? a.txt" "$(git status --porcelain)"

git add a.txt
assert_eq "basic.status.added" "A  a.txt" "$(git status --porcelain)"

git commit -q -m "first"
assert_eq "basic.blob.a" "a29bdeb434d874c9b1d8969c40c42161b03fafdc" "$(git hash-object a.txt)"
assert_eq "basic.log.oneline.count" "1" "$(git log --oneline | wc -l | tr -d ' ')"
assert_eq "basic.log.format.subject" "first" "$(git log -1 --format=%s)"
assert_eq "basic.log.format.author" "Carpet Tester <carpet@starry.os>" "$(git log -1 --format='%an <%ae>')"
assert_eq "basic.log.format.date" "1700000000" "$(git log -1 --format=%at)"
assert_eq "basic.status.clean" "" "$(git status --porcelain)"

printf 'line2\n' >> a.txt
assert_eq "basic.status.modified" " M a.txt" "$(git status --porcelain)"

DIFF=$(git diff)
assert_contains "basic.diff.hunk" "@@ -1 +1,2 @@" "$DIFF"
assert_contains "basic.diff.added" "+line2" "$DIFF"
assert_contains "basic.diff.stat" "1 file changed, 1 insertion(+)" "$(git diff --stat)"
assert_eq "basic.diff.numstat" "1	0	a.txt" "$(git diff --numstat)"

git add a.txt
git commit -q -m "second"

SHOW=$(git show HEAD)
assert_contains "basic.show.subject" "second" "$SHOW"
assert_contains "basic.show.added" "+line2" "$SHOW"

printf 'gone\n' > b.txt
git add b.txt
git commit -q -m "add b"
git rm -q b.txt
assert_no_file "basic.rm.worktree" "b.txt"
assert_eq "basic.rm.staged" "D  b.txt" "$(git status --porcelain)"
git commit -q -m "remove b"

git mv a.txt renamed.txt
assert_file "basic.mv.dest" "renamed.txt"
assert_no_file "basic.mv.src" "a.txt"
assert_contains "basic.mv.status" "R" "$(git status --porcelain)"
git commit -q -m "rename a"
assert_eq "basic.mv.tracked" "renamed.txt" "$(git ls-files)"

assert_eq "basic.rev-list.count" "5" "$(git rev-list --count HEAD)"

cd "$ROOT"

# ===========================================================================
# 3. BRANCH / CHECKOUT / SWITCH / MERGE
# ===========================================================================
echo "=== branch/merge ==="
mkdir merge && cd merge
git init -q
printf 'base\n' > f.txt
git add f.txt
git commit -q -m "base"

git branch feature
assert_contains "merge.branch.list" "feature" "$(git branch --format='%(refname:short)')"
git checkout -q feature
assert_eq "merge.checkout.head" "feature" "$(git rev-parse --abbrev-ref HEAD)"
printf 'feature\n' >> f.txt
git add f.txt
git commit -q -m "feature change"

git checkout -q master
git merge -q feature
assert_contains "merge.ff.content" "feature" "$(cat f.txt)"
assert_eq "merge.ff.linear" "2" "$(git rev-list --count HEAD)"

git switch -q -c topic
assert_eq "merge.switch.head" "topic" "$(git rev-parse --abbrev-ref HEAD)"
git switch -q master

git switch -q -c side
printf 'side-only\n' > s.txt
git add s.txt
git commit -q -m "side file"
git switch -q master
printf 'master-only\n' > m.txt
git add m.txt
git commit -q -m "master file"
git merge -q -m "merge side" side
assert_file "merge.3way.side-file" "s.txt"
assert_file "merge.3way.master-file" "m.txt"
assert_eq "merge.3way.parents" "2" "$(git cat-file -p HEAD | grep -c '^parent ')"

git switch -q -c conflictA master
printf 'AAA\n' > c.txt
git add c.txt
git commit -q -m "A"
git switch -q master
printf 'BBB\n' > c.txt
git add c.txt
git commit -q -m "B"
if git merge -q conflictA >/dev/null 2>&1; then
    fail "merge.conflict.detected: merge unexpectedly succeeded"
else
    pass
fi
CONFLICT=$(cat c.txt)
assert_contains "merge.conflict.marker.ours" "<<<<<<<" "$CONFLICT"
assert_contains "merge.conflict.marker.sep" "=======" "$CONFLICT"
assert_contains "merge.conflict.marker.theirs" ">>>>>>>" "$CONFLICT"
assert_contains "merge.conflict.status" "AA c.txt" "$(git status --porcelain)"
printf 'resolved\n' > c.txt
git add c.txt
git commit -q -m "resolve"
assert_eq "merge.conflict.resolved" "resolved" "$(cat c.txt)"
assert_eq "merge.conflict.clean" "" "$(git status --porcelain)"

cd "$ROOT"

# ===========================================================================
# 4. RESET / RESTORE
# ===========================================================================
echo "=== reset/restore ==="
mkdir reset && cd reset
git init -q
printf 'v1\n' > r.txt
git add r.txt
git commit -q -m "c1"
C1=$(git rev-parse HEAD)
printf 'v2\n' > r.txt
git add r.txt
git commit -q -m "c2"

git reset -q --soft "$C1"
assert_eq "reset.soft.head" "$C1" "$(git rev-parse HEAD)"
assert_eq "reset.soft.index" "M  r.txt" "$(git status --porcelain)"
assert_eq "reset.soft.worktree" "v2" "$(cat r.txt)"

git reset -q --mixed "$C1"
assert_eq "reset.mixed.index" " M r.txt" "$(git status --porcelain)"
assert_eq "reset.mixed.worktree" "v2" "$(cat r.txt)"

git reset -q --hard "$C1"
assert_eq "reset.hard.clean" "" "$(git status --porcelain)"
assert_eq "reset.hard.worktree" "v1" "$(cat r.txt)"

printf 'dirty\n' > r.txt
git restore r.txt
assert_eq "restore.worktree" "v1" "$(cat r.txt)"

printf 'stage-me\n' > new.txt
git add new.txt
git restore --staged new.txt
assert_eq "restore.staged" "?? new.txt" "$(git status --porcelain)"
rm -f new.txt

cd "$ROOT"

# ===========================================================================
# 5. REBASE / CHERRY-PICK / REVERT
# ===========================================================================
echo "=== rebase/cherry-pick/revert ==="
mkdir rebase && cd rebase
git init -q
printf '1\n' > f.txt; git add f.txt; git commit -q -m "c1"
printf '2\n' >> f.txt; git add f.txt; git commit -q -m "c2"
git switch -q -c work
printf '3\n' >> f.txt; git add f.txt; git commit -q -m "c3"
printf '4\n' >> f.txt; git add f.txt; git commit -q -m "c4"
git switch -q master
printf 'x\n' > other.txt; git add other.txt; git commit -q -m "c-master"

git switch -q work
git rebase -q master >/dev/null 2>&1
assert_file "rebase.plain.brought-other" "other.txt"
assert_eq "rebase.plain.subject" "c4" "$(git log -1 --format=%s)"
assert_contains "rebase.plain.log" "c-master" "$(git log --format=%s)"

# interactive rebase: fixup c4 into c3 via GIT_SEQUENCE_EDITOR (no prompt)
export GIT_SEQUENCE_EDITOR='sed -i -e "2s/^pick/fixup/"'
export GIT_EDITOR=true
BEFORE=$(git rev-list --count HEAD)
git rebase -q -i HEAD~2 >/dev/null 2>&1
AFTER=$(git rev-list --count HEAD)
assert_eq "rebase.interactive.fixup" "$((BEFORE - 1))" "$AFTER"
unset GIT_SEQUENCE_EDITOR
unset GIT_EDITOR

git switch -q -c featbase master
printf 'fb\n' > fb.txt; git add fb.txt; git commit -q -m "fb"
git switch -q -c feattop
printf 'ft\n' > ft.txt; git add ft.txt; git commit -q -m "ft"
git rebase -q --onto master featbase feattop >/dev/null 2>&1
assert_file "rebase.onto.top-file" "ft.txt"
assert_no_file "rebase.onto.excluded-base" "fb.txt"

git switch -q master
git switch -q -c cp
printf 'pickme\n' > pick.txt; git add pick.txt; git commit -q -m "pickme"
PICK=$(git rev-parse HEAD)
git switch -q master
git cherry-pick "$PICK" >/dev/null 2>&1
assert_file "cherry-pick.applied" "pick.txt"
assert_eq "cherry-pick.subject" "pickme" "$(git log -1 --format=%s)"

git revert --no-edit HEAD >/dev/null 2>&1
assert_no_file "revert.removed-file" "pick.txt"
assert_contains "revert.subject" "Revert" "$(git log -1 --format=%s)"

cd "$ROOT"

# ===========================================================================
# 6. STASH
# ===========================================================================
echo "=== stash ==="
mkdir stash && cd stash
git init -q
printf 'base\n' > s.txt; git add s.txt; git commit -q -m "base"
printf 'wip\n' >> s.txt
git stash push -q -m "wip stash"
assert_eq "stash.push.clean" "base" "$(cat s.txt)"
assert_eq "stash.push.status" "" "$(git status --porcelain)"
assert_contains "stash.list" "wip stash" "$(git stash list)"
git stash pop -q >/dev/null 2>&1
assert_contains "stash.pop.content" "wip" "$(cat s.txt)"
assert_eq "stash.pop.emptied" "" "$(git stash list)"

git stash push -q -m "second"
git stash apply -q >/dev/null 2>&1
assert_contains "stash.apply.content" "wip" "$(cat s.txt)"
assert_eq "stash.apply.retained" "1" "$(git stash list | wc -l | tr -d ' ')"
git stash drop -q >/dev/null 2>&1
assert_eq "stash.drop.emptied" "" "$(git stash list)"

cd "$ROOT"

# ===========================================================================
# 7. TAG / DESCRIBE
# ===========================================================================
echo "=== tag/describe ==="
mkdir tag && cd tag
git init -q
printf 'v\n' > v.txt; git add v.txt; git commit -q -m "c1"
git tag light
assert_contains "tag.light.list" "light" "$(git tag -l)"
assert_eq "tag.light.type" "commit" "$(git cat-file -t light)"

git tag -a v1.0 -m "release one"
assert_contains "tag.annotated.list" "v1.0" "$(git tag -l)"
assert_eq "tag.annotated.type" "tag" "$(git cat-file -t v1.0)"
TAGOBJ=$(git cat-file -p v1.0)
assert_contains "tag.annotated.tagger" "Carpet Tester" "$TAGOBJ"
assert_contains "tag.annotated.message" "release one" "$TAGOBJ"

assert_eq "describe.exact" "v1.0" "$(git describe --tags)"
printf 'more\n' >> v.txt; git add v.txt; git commit -q -m "c2"
assert_contains "describe.ahead" "v1.0-1-g" "$(git describe --tags)"

cd "$ROOT"

# ===========================================================================
# 8. TRANSPORT
# ===========================================================================
echo "=== transport ==="
mkdir transport && cd transport
git init -q origin
cd origin
printf 'shared\n' > data.txt; git add data.txt; git commit -q -m "origin c1"
cd "$ROOT/transport"

git clone -q "$ROOT/transport/origin" cloned
assert_file "clone.file" "cloned/data.txt"
assert_eq "clone.content" "shared" "$(cat cloned/data.txt)"
git clone -q "file://$ROOT/transport/origin" clonedurl
assert_eq "clone.url.content" "shared" "$(cat clonedurl/data.txt)"
assert_contains "clone.remote" "origin" "$(git -C cloned remote)"

git init -q --bare central.git
git -C origin remote add central "$ROOT/transport/central.git"
git -C origin push -q central master
git clone -q central.git consumer
assert_eq "push.consumer.content" "shared" "$(cat consumer/data.txt)"

printf 'update\n' >> origin/data.txt
git -C origin add data.txt
git -C origin commit -q -m "origin c2"
git -C origin push -q central master
git -C consumer pull -q --ff-only origin master >/dev/null 2>&1
assert_contains "pull.consumer.updated" "update" "$(cat consumer/data.txt)"

printf 'third\n' >> origin/data.txt
git -C origin add data.txt
git -C origin commit -q -m "origin c3"
git -C origin push -q central master
git -C consumer fetch -q origin
assert_eq "fetch.count" "3" "$(git -C consumer rev-list --count origin/master)"

git -C origin bundle create "$ROOT/transport/repo.bundle" --all >/dev/null 2>&1
assert_ok "bundle.verify" git -C origin bundle verify "$ROOT/transport/repo.bundle"
git clone -q "$ROOT/transport/repo.bundle" from-bundle
assert_eq "bundle.clone.content" "shared" "$(head -1 from-bundle/data.txt)"

git -C origin archive --format=tar -o "$ROOT/transport/arc.tar" HEAD
assert_contains "archive.tar.contents" "data.txt" "$(tar -tf "$ROOT/transport/arc.tar")"

cd "$ROOT"

# ===========================================================================
# 9. WORKTREE
# ===========================================================================
echo "=== worktree ==="
mkdir wt && cd wt
git init -q
printf 'main\n' > w.txt; git add w.txt; git commit -q -m "c1"
git worktree add -q ../wt-linked -b linked >/dev/null 2>&1
assert_file "worktree.add.file" "../wt-linked/w.txt"
assert_contains "worktree.list" "wt-linked" "$(git worktree list)"
assert_eq "worktree.branch" "linked" "$(git -C ../wt-linked rev-parse --abbrev-ref HEAD)"
git worktree remove ../wt-linked
assert_no_file "worktree.remove" "../wt-linked/w.txt"

cd "$ROOT"

# ===========================================================================
# 10. SUBMODULE
# ===========================================================================
echo "=== submodule ==="
mkdir sub && cd sub
git init -q subdep
cd subdep
printf 'dep\n' > dep.txt; git add dep.txt; git commit -q -m "dep c1"
cd "$ROOT/sub"
git init -q super
cd super
printf 'top\n' > top.txt; git add top.txt; git commit -q -m "super c1"
git -c protocol.file.allow=always submodule add -q "$ROOT/sub/subdep" vendor/dep >/dev/null 2>&1
assert_file "submodule.gitmodules" ".gitmodules"
assert_file "submodule.checked-out" "vendor/dep/dep.txt"
assert_contains "submodule.gitmodules.path" "path = vendor/dep" "$(cat .gitmodules)"
git commit -q -m "add submodule"
assert_contains "submodule.status" "vendor/dep" "$(git submodule status)"
assert_ok "submodule.update" git -c protocol.file.allow=always submodule update --init

cd "$ROOT"

# ===========================================================================
# 11. SPARSE-CHECKOUT (cone)
# ===========================================================================
echo "=== sparse-checkout ==="
mkdir sparse && cd sparse
git init -q
mkdir -p keep drop
printf 'keep\n' > keep/k.txt
printf 'drop\n' > drop/d.txt
git add .
git commit -q -m "two dirs"
git sparse-checkout init --cone >/dev/null 2>&1
git sparse-checkout set keep >/dev/null 2>&1
assert_file "sparse.keep.present" "keep/k.txt"
assert_no_file "sparse.drop.absent" "drop/d.txt"
git sparse-checkout disable >/dev/null 2>&1
assert_file "sparse.disable.restores" "drop/d.txt"

cd "$ROOT"

# ===========================================================================
# 12. REFLOG
# ===========================================================================
echo "=== reflog ==="
mkdir reflog && cd reflog
git init -q
printf '1\n' > r.txt; git add r.txt; git commit -q -m "c1"
printf '2\n' >> r.txt; git add r.txt; git commit -q -m "c2"
git reset -q --hard HEAD~1
assert_contains "reflog.records-reset" "reset:" "$(git reflog)"
LOST=$(git reflog | grep 'commit: c2' | head -1 | cut -d' ' -f1)
assert_ok "reflog.recover" git cat-file -t "$LOST"

cd "$ROOT"

# ===========================================================================
# 13. BLAME
# ===========================================================================
echo "=== blame ==="
mkdir blame && cd blame
git init -q
printf 'first line\n' > code.txt
git add code.txt; git commit -q -m "add first"
printf 'second line\n' >> code.txt
git add code.txt; git commit -q -m "add second"
BLAME=$(git blame --line-porcelain code.txt)
assert_contains "blame.author" "author Carpet Tester" "$BLAME"
assert_contains "blame.line1" "first line" "$BLAME"
assert_contains "blame.line2" "second line" "$BLAME"
NCOMMITS=$(git blame --line-porcelain code.txt | grep -c '^author-time ')
assert_eq "blame.two-lines" "2" "$NCOMMITS"

cd "$ROOT"

# ===========================================================================
# 14. BISECT (scripted good/bad)
# ===========================================================================
echo "=== bisect ==="
mkdir bisect && cd bisect
git init -q
printf 'ok\n' > flag.txt; git add flag.txt; git commit -q -m "good1"
GOOD=$(git rev-parse HEAD)
printf 'ok\n' > flag.txt; echo x >> flag.txt; git add flag.txt; git commit -q -m "good2"
printf 'BUG\n' > flag.txt; git add flag.txt; git commit -q -m "bug"
BADCOMMIT=$(git rev-parse HEAD)
printf 'BUG\n' > flag.txt; echo y >> flag.txt; git add flag.txt; git commit -q -m "after1"
printf 'BUG\n' > flag.txt; echo z >> flag.txt; git add flag.txt; git commit -q -m "after2"
BAD=$(git rev-parse HEAD)
cat > /tmp/git-bisect-run.sh <<'EOF'
#!/bin/sh
head -1 flag.txt | grep -q BUG && exit 1
exit 0
EOF
chmod +x /tmp/git-bisect-run.sh
git bisect start "$BAD" "$GOOD" >/dev/null 2>&1
RESULT=$(git bisect run /tmp/git-bisect-run.sh 2>/dev/null)
git bisect reset >/dev/null 2>&1
assert_contains "bisect.found" "$BADCOMMIT" "$RESULT"
assert_contains "bisect.found.subject" "bug" "$RESULT"

cd "$ROOT"

# ===========================================================================
# 15. RERERE
# ===========================================================================
echo "=== rerere ==="
mkdir rerere && cd rerere
git init -q
git config rerere.enabled true
printf 'base\n' > x.txt; git add x.txt; git commit -q -m "base"
git switch -q -c br1
printf 'one\n' > x.txt; git add x.txt; git commit -q -m "one"
git switch -q master
printf 'two\n' > x.txt; git add x.txt; git commit -q -m "two"
git merge br1 >/dev/null 2>&1 || true
assert_contains "rerere.conflict" "<<<<<<<" "$(cat x.txt)"
printf 'reconciled\n' > x.txt
git add x.txt
git rerere status >/dev/null 2>&1
git commit -q -m "merge resolved"
git reset -q --hard HEAD~1
git merge br1 >/dev/null 2>&1 || true
assert_eq "rerere.autoresolved" "reconciled" "$(cat x.txt)"
git merge --abort >/dev/null 2>&1 || true

cd "$ROOT"

# ===========================================================================
# 16. GC / FSCK
# ===========================================================================
echo "=== gc/fsck ==="
mkdir gc && cd gc
git init -q
printf 'a\n' > a.txt; git add a.txt; git commit -q -m "c1"
printf 'b\n' >> a.txt; git add a.txt; git commit -q -m "c2"
git gc -q >/dev/null 2>&1
FSCK=$(git fsck --full 2>&1)
assert_not_contains "fsck.no-missing" "missing" "$FSCK"
assert_not_contains "fsck.no-corrupt" "corrupt" "$FSCK"
assert_eq "gc.history-intact" "2" "$(git rev-list --count HEAD)"

cd "$ROOT"

# ===========================================================================
# 17. CONFIG
# ===========================================================================
echo "=== config ==="
mkdir cfg && cd cfg
git init -q
git config --local carpet.key "carpet-value"
assert_eq "config.local.get" "carpet-value" "$(git config --local --get carpet.key)"
git config --local carpet.key "new-value"
assert_eq "config.local.overwrite" "new-value" "$(git config carpet.key)"
git config --local --unset carpet.key
assert_fails "config.local.unset" git config --get carpet.key

cd "$ROOT"

# ===========================================================================
# 18. NOTES
# ===========================================================================
echo "=== notes ==="
mkdir notes && cd notes
git init -q
printf 'n\n' > n.txt; git add n.txt; git commit -q -m "c1"
git notes add -m "a carpet note" HEAD
assert_eq "notes.show" "a carpet note" "$(git notes show HEAD)"
assert_contains "notes.in-log" "a carpet note" "$(git log -1 --format=%N)"
git notes remove HEAD >/dev/null 2>&1
assert_fails "notes.removed" git notes show HEAD

cd "$ROOT"

# ===========================================================================
# 19. FORMAT-PATCH / AM (round-trip)
# ===========================================================================
echo "=== format-patch/am ==="
mkdir patch && cd patch
git init -q
printf 'l1\n' > p.txt; git add p.txt; git commit -q -m "c1"
printf 'l2\n' >> p.txt; git add p.txt; git commit -q -m "add l2"
git format-patch -q -1 -o /tmp/git-patches HEAD >/dev/null 2>&1
PATCHFILE=$(ls /tmp/git-patches/*.patch | head -1)
assert_file "format-patch.file" "$PATCHFILE"
assert_contains "format-patch.subject" "add l2" "$(cat "$PATCHFILE")"

git init -q "$ROOT/patch/target"
cd "$ROOT/patch/target"
printf 'l1\n' > p.txt; git add p.txt; git commit -q -m "c1"
git am /tmp/git-patches/*.patch >/dev/null 2>&1
assert_contains "am.applied.content" "l2" "$(cat p.txt)"
assert_eq "am.applied.subject" "add l2" "$(git log -1 --format=%s)"
assert_eq "am.applied.author" "Carpet Tester" "$(git log -1 --format=%an)"

cd "$ROOT"

# ===========================================================================
# 20. SHORTLOG
# ===========================================================================
echo "=== shortlog ==="
mkdir slog && cd slog
git init -q
printf '1\n' > s.txt; git add s.txt; git commit -q -m "one"
printf '2\n' >> s.txt; git add s.txt; git commit -q -m "two"
printf '3\n' >> s.txt; git add s.txt; git commit -q -m "three"
SLOG=$(git shortlog -s -n HEAD)
assert_contains "shortlog.author" "Carpet Tester" "$SLOG"
assert_contains "shortlog.count" "3" "$SLOG"

cd "$ROOT"

finish
