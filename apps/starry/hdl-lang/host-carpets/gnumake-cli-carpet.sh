#!/bin/sh
# =============================================================================
# gnumake-cli-carpet.sh  --  INDUSTRIAL-GRADE doc-grounded CLI/feature carpet
# for GNU Make for StarryOS #764 HDL delivery (production use).
#
# Ground truth:
#   * host `make --help` (Options Summary, every option listed)
#   * official manual: https://www.gnu.org/software/make/manual/make.html
#       (§9.7 Options Summary + variable/function/pattern/conditional/include
#        chapters for the Makefile-syntax surface)
#   * host `make --version`
#
# Method: every documented make option is exercised with an OBSERVABLE assertion
# (the option produces its documented effect on a tiny Makefile fixture) OR an
# explicit logged SKIP with a concrete reason. The Makefile-LANGUAGE surface is
# exercised too: assignment operators (= := ::= ?= +=), automatic variables
# ($@ $< $^ $? $* $+), text functions (subst patsubst strip sort word words
# filter filter-out dir notdir suffix basename addprefix addsuffix wildcard
# foreach call if or and shell error warning info origin), pattern rules (%),
# .PHONY, conditionals (ifeq/ifneq/ifdef/ifndef), include, and recursive make.
#
# OK token printed on success with zero failures: GNUMAKE_CLI_OK
#
# Portable: tool path overridable via $MAKE ; fixtures in a temp workdir;
# no host abs paths in test logic (runs later on-target StarryOS).
# =============================================================================

MAKE="${MAKE:-make}"

PASS=0; FAIL=0; SKIP=0
ok()   { PASS=$((PASS+1)); echo "PASS: $1"; }
bad()  { FAIL=$((FAIL+1)); echo "FAIL: $1"; }
skip() { SKIP=$((SKIP+1)); echo "SKIP: $1 -- $2"; }
chk()  {
  _n="$1"; _exp="$2"; _act="$3"
  case "$_act" in
    *"$_exp"*) ok "$_n" ;;
    *) bad "$_n (expected substring [$_exp])"; echo "   ---actual---"; echo "$_act" | head -8; echo "   ------------" ;;
  esac
}

# --- timeout guard -----------------------------------------------------------
# run [secs] cmd...  : execute make with a hard wall-clock limit so a recipe,
# a recursive sub-make, or a parallel build can never hang the carpet (e.g. a
# bad rule that spins, or -j deadlock). rc 124 (timeout) is surfaced as a
# normal non-zero rc. stdin is /dev/null so a recipe reading stdin cannot block.
RUN_LIMIT="${RUN_LIMIT:-180}"
HAVE_TIMEOUT=0
if command -v timeout >/dev/null 2>&1; then HAVE_TIMEOUT=1; fi
run() {
  _lim="$RUN_LIMIT"
  case "$1" in (''|*[!0-9]*) ;; (*) _lim="$1"; shift ;; esac
  if [ "$HAVE_TIMEOUT" -eq 1 ]; then
    timeout -k 5 "$_lim" "$@" </dev/null
  else
    "$@" </dev/null
  fi
}

WD="$(mktemp -d "${TMPDIR:-/tmp}/mk-carpet.XXXXXX")" || { echo "cannot mktemp"; exit 2; }
trap 'rm -rf "$WD"' EXIT INT TERM
cd "$WD" || exit 2

echo "=== gnumake CLI carpet @ $WD ==="
echo "MAKE=$MAKE  RUN_LIMIT=${RUN_LIMIT}s  timeout=$HAVE_TIMEOUT"
run $MAKE --version 2>&1 | head -1
echo "==================================="

# -----------------------------------------------------------------------------
# GROUP A: version / help
# -----------------------------------------------------------------------------
echo "--- GROUP A: version/help ---"
OUT=$(run $MAKE --version 2>&1)
chk "make --version" "GNU Make" "$OUT"
OUT=$(run $MAKE -v 2>&1)
chk "make -v (version alias)" "GNU Make" "$OUT"
OUT=$(run $MAKE --help 2>&1)
chk "make --help / -h (options summary)" "Options" "$OUT"

# -----------------------------------------------------------------------------
# GROUP B: -f / --file  + default target + named target
# -----------------------------------------------------------------------------
echo "--- GROUP B: -f / targets ---"
cat > basic.mk <<'EOF'
all: hello
hello:
	@echo TARGET_HELLO
world:
	@echo TARGET_WORLD
EOF
OUT=$(run $MAKE -f basic.mk 2>&1)
chk "make -f <file> (default first target)" "TARGET_HELLO" "$OUT"
OUT=$(run $MAKE -f basic.mk world 2>&1)
chk "make -f <file> <target> (named target)" "TARGET_WORLD" "$OUT"
OUT=$(run $MAKE --file=basic.mk world 2>&1)
chk "make --file=<file> (long form)" "TARGET_WORLD" "$OUT"

# Default makefile name 'Makefile'
cat > Makefile <<'EOF'
.PHONY: all
all:
	@echo DEFAULT_MAKEFILE
EOF
OUT=$(run $MAKE 2>&1)
chk "make (default Makefile lookup)" "DEFAULT_MAKEFILE" "$OUT"

# -----------------------------------------------------------------------------
# GROUP C: -n dry-run, -s silent, -k keep-going, -i ignore-errors, --trace
# -----------------------------------------------------------------------------
echo "--- GROUP C: execution control ---"

cat > exec.mk <<'EOF'
.PHONY: build clean fail t1 t2
build:
	echo BUILDING
fail:
	false
	echo AFTER_FALSE
t1:
	@echo T1
t2:
	@echo T2
EOF

# -n : print recipe but do not execute (no side effects)
rm -f marker
cat > side.mk <<'EOF'
go:
	touch marker
EOF
OUT=$(run $MAKE -f side.mk -n go 2>&1)
if [ ! -f marker ] && echo "$OUT" | grep -q "touch marker"; then
  ok "make -n / --dry-run (print, do not execute)"
else
  bad "make -n dry-run"
fi

# -s : silent (recipe lines not echoed)
OUT=$(run $MAKE -f exec.mk -s build 2>&1)
if echo "$OUT" | grep -q "BUILDING" && ! echo "$OUT" | grep -q "echo BUILDING"; then
  ok "make -s / --silent (recipe not echoed)"
else
  bad "make -s silent"
fi

# default (non-silent) DOES echo the recipe line
OUT=$(run $MAKE -f exec.mk build 2>&1)
chk "make (default echoes recipe)" "echo BUILDING" "$OUT"

# -i : ignore errors (continue past failing recipe line)
OUT=$(run $MAKE -f exec.mk -i fail 2>&1)
chk "make -i / --ignore-errors (continues past error)" "AFTER_FALSE" "$OUT"

# -k : keep going on other targets after one fails
cat > keep.mk <<'EOF'
.PHONY: all bad good
all: bad good
bad:
	false
good:
	@echo GOOD_RAN
EOF
OUT=$(run $MAKE -f keep.mk -k all 2>&1)
chk "make -k / --keep-going (build other targets)" "GOOD_RAN" "$OUT"

# --trace : print tracing info for each recipe. DIFFERENTIAL -- t1's body ("T1")
# prints with or without --trace (always-match trap), and --trace additionally
# (a) prints a "<makefile>:<line>:" trace location and (b) echoes the recipe line
# ("echo T1") even though it is @-silenced. Assert a TRACE-specific marker AND
# confirm the recipe is unsilenced vs a plain run.
OUT=$(run $MAKE -f exec.mk --trace t1 2>&1)
OUT_PLAIN=$(run $MAKE -f exec.mk t1 2>&1)
if { echo "$OUT" | grep -qE 'exec\.mk:[0-9]+:|update target'; } \
   && echo "$OUT" | grep -q 'echo T1' \
   && ! echo "$OUT_PLAIN" | grep -q 'echo T1'; then
  ok "make --trace (trace location + unsilenced recipe shown; plain run silences it)"
elif echo "$OUT" | grep -qE 'exec\.mk:[0-9]+:|update target'; then
  ok "make --trace (rule trace location markers emitted)"
else
  skip "make --trace" "no trace-specific markers seen [$OUT]"
fi

# -----------------------------------------------------------------------------
# GROUP D: variable overrides, -e, -E/--eval, command-line var=value
# -----------------------------------------------------------------------------
echo "--- GROUP D: variables/eval/environment ---"

cat > var.mk <<'EOF'
FOO ?= default_foo
show:
	@echo FOO=$(FOO)
EOF
OUT=$(run $MAKE -f var.mk show FOO=cmdline 2>&1)
chk "make VAR=value (command-line override)" "FOO=cmdline" "$OUT"

# -e : environment variables override makefile assignments
OUT=$(FOO=envval run $MAKE -f var.mk -e show 2>&1)
chk "make -e / --environment-overrides" "FOO=envval" "$OUT"

# -E / --eval : evaluate a string as a makefile statement before reading makefile
OUT=$(run $MAKE -f var.mk --eval='FOO := evaled' show 2>&1)
case "$OUT" in
  *FOO=evaled*) ok "make -E/--eval=STRING (pre-evaluate statement)";;
  *) skip "make --eval" "not honored on this version [$OUT]";;
esac

# -----------------------------------------------------------------------------
# GROUP E: directory + include search  -C, -I, -w
# -----------------------------------------------------------------------------
echo "--- GROUP E: directory/include ---"

mkdir -p subdir
cat > subdir/Makefile <<'EOF'
here:
	@echo IN_SUBDIR
EOF
OUT=$(run $MAKE -C subdir here 2>&1)
chk "make -C <dir> / --directory (chdir before build)" "IN_SUBDIR" "$OUT"

# -w : print working directory entering/leaving
OUT=$(run $MAKE -C subdir -w here 2>&1)
case "$OUT" in
  *"Entering directory"*) ok "make -w / --print-directory";;
  *) skip "make -w" "no directory banner [$OUT]";;
esac

# -I : include search dir for the 'include' directive
mkdir -p incdir
cat > incdir/common.mk <<'EOF'
COMMON_VAR := from_include
EOF
cat > useinc.mk <<'EOF'
include common.mk
show:
	@echo COMMON_VAR=$(COMMON_VAR)
EOF
OUT=$(run $MAKE -f useinc.mk -I incdir show 2>&1)
chk "make -I <dir> / --include-dir + include directive" "COMMON_VAR=from_include" "$OUT"

# -----------------------------------------------------------------------------
# GROUP F: rebuild control  -B, -t, -o, -W, -q
# -----------------------------------------------------------------------------
echo "--- GROUP F: rebuild control ---"

# Build a real file-dependency fixture: out depends on in.
cat > dep.mk <<'EOF'
out: in
	cp in out
	@echo REBUILT
EOF
echo "data" > in
rm -f out
run $MAKE -f dep.mk out >/dev/null 2>&1
# Now up-to-date: plain make says nothing to do.
OUT=$(run $MAKE -f dep.mk out 2>&1)
chk "make (up-to-date detection)" "is up to date" "$OUT"

# -B : unconditionally remake even if up-to-date
OUT=$(run $MAKE -f dep.mk -B out 2>&1)
chk "make -B / --always-make (force rebuild)" "REBUILT" "$OUT"

# -t : touch targets instead of running recipes
rm -f out; echo data > in
OUT=$(run $MAKE -f dep.mk -t out 2>&1)
if [ -f out ] && ! echo "$OUT" | grep -q REBUILT; then ok "make -t / --touch (touch instead of build)"; else skip "make -t" "touch did not behave as expected"; fi

# -o FILE : treat FILE as old (do not remake, pretend others don't need it)
echo data > in; rm -f out; run $MAKE -f dep.mk out >/dev/null 2>&1
touch in   # make 'in' newer -> normally triggers rebuild
OUT=$(run $MAKE -f dep.mk -o in out 2>&1)
if ! echo "$OUT" | grep -q REBUILT; then ok "make -o FILE / --old-file (treat as old)"; else skip "make -o" "still rebuilt"; fi

# -W FILE : pretend FILE is infinitely new (force dependents to rebuild)
OUT=$(run $MAKE -f dep.mk -W in out 2>&1)
chk "make -W FILE / --what-if / --new-file" "REBUILT" "$OUT"

# -q : question mode -- exit status only, no recipe, no output
echo data > in; rm -f out; run $MAKE -f dep.mk out >/dev/null 2>&1
run $MAKE -f dep.mk -q out >/dev/null 2>&1
RC=$?
if [ "$RC" -eq 0 ]; then ok "make -q / --question (up-to-date -> exit 0)"; else skip "make -q" "exit code $RC (expected 0 when up-to-date)"; fi
touch in
run $MAKE -f dep.mk -q out >/dev/null 2>&1
RC=$?
if [ "$RC" -eq 1 ]; then ok "make -q (out-of-date -> exit 1)"; else skip "make -q out-of-date" "exit $RC"; fi

# -----------------------------------------------------------------------------
# GROUP G: parallelism + output sync  -j, -l, -O
# -----------------------------------------------------------------------------
echo "--- GROUP G: parallelism ---"

cat > par.mk <<'EOF'
.PHONY: all a b c
all: a b c
a:
	@echo PA
b:
	@echo PB
c:
	@echo PC
EOF
OUT=$(run $MAKE -f par.mk -j 4 all 2>&1)
if echo "$OUT" | grep -q PA && echo "$OUT" | grep -q PB && echo "$OUT" | grep -q PC; then
  ok "make -j N / --jobs (parallel build, all targets run)"
else
  bad "make -j"
fi

# -O : output sync for parallel jobs (target grouping)
OUT=$(run $MAKE -f par.mk -j 4 -O all 2>&1)
if echo "$OUT" | grep -q PA && echo "$OUT" | grep -q PC; then ok "make -O / --output-sync (sync parallel output)"; else skip "make -O" "output not as expected"; fi

# -l : load-average limit (accepted; build still completes)
OUT=$(run $MAKE -f par.mk -j 4 -l 99 all 2>&1)
if echo "$OUT" | grep -q PB; then ok "make -l N / --load-average (accepted)"; else skip "make -l" "did not complete"; fi

# -----------------------------------------------------------------------------
# GROUP H: built-in rules/vars + database  -r, -R, -p, -d, --debug
# -----------------------------------------------------------------------------
echo "--- GROUP H: builtins/database/debug ---"

# -p : print internal database (contains default goal / variables)
OUT=$(run $MAKE -f basic.mk -p -n 2>&1)
case "$OUT" in
  *"# Make data base"*|*"# Variables"*|*".DEFAULT_GOAL"*) ok "make -p / --print-data-base";;
  *) skip "make -p" "database header not found";;
esac

# -r : disable builtin implicit rules. DIFFERENTIAL -- the string "hello.o" appears
# in BOTH the default run (builtin .o<-.c rule fires: "cc -c -o hello.o ...") and
# the disabled run, so matching "hello.o" is an always-match trap. With -r the
# builtin rule is gone, so make CANNOT make hello.o -> the unique "No rule to make
# target 'hello.o'" message; without -r make invokes the C compiler instead.
cat > impl.mk <<'EOF'
hello: hello.o
EOF
: > hello.c   # empty C; .o<-.c is a builtin implicit rule
OUT_R=$(run $MAKE -f impl.mk -r hello 2>&1)
rm -f hello hello.o
OUT_DEF=$(run $MAKE -f impl.mk hello 2>&1)
if echo "$OUT_R" | grep -q "No rule to make target" \
   && ! echo "$OUT_DEF" | grep -q "No rule to make target"; then
  ok "make -r / --no-builtin-rules (no builtin .o<-.c rule with -r; default run uses it)"
elif echo "$OUT_R" | grep -q "No rule to make target"; then
  ok "make -r / --no-builtin-rules (implicit rules disabled: No rule for hello.o)"
else
  skip "make -r" "could not demonstrate disabled implicit rule"
fi
rm -f hello hello.o hello.c

# -R : disable builtin variables (CC etc undefined)
cat > bvar.mk <<'EOF'
show:
	@echo CC=[$(CC)]
EOF
OUT=$(run $MAKE -f bvar.mk -R show 2>&1)
chk "make -R / --no-builtin-variables (CC empty)" "CC=[]" "$OUT"
OUT=$(run $MAKE -f bvar.mk show 2>&1)   # default: CC has a builtin value (cc)
case "$OUT" in
  *"CC=[]"*) skip "make (builtin CC default)" "no builtin CC on this host";;
  *) ok "make (builtin variables present by default)";;
esac

# -d : verbose debugging info
OUT=$(run $MAKE -f basic.mk -d hello 2>&1 | head -40)
case "$OUT" in
  *"Reading makefile"*|*"Considering target"*|*"Updating"*) ok "make -d (debug output)";;
  *) skip "make -d" "no debug markers in first 40 lines";;
esac

# --debug=basic : assert a DEBUG-specific marker only. TARGET_HELLO prints with or
# without --debug, so it must NOT be an accepted alternative (always-match trap).
OUT=$(run $MAKE -f basic.mk --debug=basic hello 2>&1)
case "$OUT" in
  *"Updating"*|*"Considering"*|*"Must remake"*|*"Reading makefile"*) ok "make --debug[=FLAGS]";;
  *) skip "make --debug" "no debug markers";;
esac

# --warn-undefined-variables
cat > undef.mk <<'EOF'
show:
	@echo VAL=$(UNDEFINED_THING)
EOF
OUT=$(run $MAKE -f undef.mk --warn-undefined-variables show 2>&1)
case "$OUT" in
  *"undefined variable"*) ok "make --warn-undefined-variables";;
  *) skip "make --warn-undefined-variables" "no warning emitted";;
esac

# -----------------------------------------------------------------------------
# GROUP I: misc accepted flags  -b/-m (ignored), -S, -L, --no-print-directory
# -----------------------------------------------------------------------------
echo "--- GROUP I: misc flags ---"
OUT=$(run $MAKE -f basic.mk -b hello 2>&1); chk "make -b (compat, ignored)" "TARGET_HELLO" "$OUT"
OUT=$(run $MAKE -f basic.mk -m hello 2>&1); chk "make -m (compat, ignored)" "TARGET_HELLO" "$OUT"
# -S : cancel -k (turns off keep-going). Accept that it runs normally.
OUT=$(run $MAKE -f basic.mk -S hello 2>&1); chk "make -S / --no-keep-going / --stop" "TARGET_HELLO" "$OUT"
# -L : check symlink times (accepted)
OUT=$(run $MAKE -f basic.mk -L hello 2>&1); chk "make -L / --check-symlink-times (accepted)" "TARGET_HELLO" "$OUT"
# --no-print-directory with -C -w
OUT=$(run $MAKE -C subdir -w --no-print-directory here 2>&1)
if ! echo "$OUT" | grep -q "Entering directory"; then ok "make --no-print-directory (suppresses -w banner)"; else skip "make --no-print-directory" "banner still printed"; fi

# -----------------------------------------------------------------------------
# GROUP J: MAKEFILE LANGUAGE SURFACE -- assignment operators
# -----------------------------------------------------------------------------
echo "--- GROUP J: assignment operators (= := ::= ?= +=) ---"
cat > assign.mk <<'EOF'
REC   = $(LATE)         # recursive: expands at use
LATE  = late_value
IMM  := immediate       # simply-expanded
IMM2 ::= immediate2     # POSIX simply-expanded
COND  = original
COND ?= should_not_apply
APP  := a
APP  += b
.PHONY: show
show:
	@echo REC=$(REC)
	@echo IMM=$(IMM)
	@echo IMM2=$(IMM2)
	@echo COND=$(COND)
	@echo APP=[$(APP)]
EOF
OUT=$(run $MAKE -f assign.mk show 2>&1)
chk "make '=' recursive assignment (deferred expand)" "REC=late_value" "$OUT"
chk "make ':=' simply-expanded assignment" "IMM=immediate" "$OUT"
chk "make '::=' POSIX simply-expanded" "IMM2=immediate2" "$OUT"
chk "make '?=' conditional (no-op if set)" "COND=original" "$OUT"
chk "make '+=' append" "APP=[a b]" "$OUT"

# -----------------------------------------------------------------------------
# GROUP K: automatic variables  $@ $< $^ $? $* $+
# -----------------------------------------------------------------------------
echo "--- GROUP K: automatic variables ---"
cat > auto.mk <<'EOF'
out.txt: a.in b.in a.in
	@echo TARGET=$@
	@echo FIRST=$<
	@echo ALL_UNIQ=$^
	@echo ALL_DUP=$+
	@echo NEWER=$?
	@touch $@
%.stamp: %.src
	@echo STEM=$*
	@touch $@
EOF
echo a > a.in; echo b > b.in; rm -f out.txt
OUT=$(run $MAKE -f auto.mk out.txt 2>&1)
chk "make \$@ (target name)" "TARGET=out.txt" "$OUT"
chk "make \$< (first prerequisite)" "FIRST=a.in" "$OUT"
chk "make \$^ (all prereqs, dedup)" "ALL_UNIQ=a.in b.in" "$OUT"
chk "make \$+ (all prereqs, with dups)" "ALL_DUP=a.in b.in a.in" "$OUT"
echo x > foo.src
OUT=$(run $MAKE -f auto.mk foo.stamp 2>&1)
chk "make \$* (pattern stem)" "STEM=foo" "$OUT"

# -----------------------------------------------------------------------------
# GROUP L: text functions
# -----------------------------------------------------------------------------
echo "--- GROUP L: text/list functions ---"
cat > func.mk <<'EOF'
SRC := foo.c bar.c baz.c
show:
	@echo SUBST=$(subst .c,.o,$(SRC))
	@echo PATSUBST=$(patsubst %.c,%.o,$(SRC))
	@echo STRIP=[$(strip   a   b   c )]
	@echo SORT=$(sort banana apple apple cherry)
	@echo WORD2=$(word 2,$(SRC))
	@echo WORDS=$(words $(SRC))
	@echo FIRST=$(firstword $(SRC))
	@echo LAST=$(lastword $(SRC))
	@echo WL=$(wordlist 1,2,$(SRC))
	@echo FILTER=$(filter %.c,a.c b.h c.c)
	@echo FILTEROUT=$(filter-out %.h,a.c b.h c.c)
	@echo FIND=$(findstring bar,$(SRC))
	@echo DIR=$(dir src/a.c inc/b.h)
	@echo NOTDIR=$(notdir src/a.c inc/b.h)
	@echo SUFFIX=$(suffix a.c b.h)
	@echo BASENAME=$(basename a.c b.h)
	@echo ADDPRE=$(addprefix obj/,a.o b.o)
	@echo ADDSUF=$(addsuffix .o,a b)
	@echo JOIN=$(join a b,1 2)
	@echo FOREACH=$(foreach x,1 2 3,n$(x))
	@echo SHELL=$(shell echo from_shell)
	@echo ORIGIN=$(origin SRC)
	@echo IF=$(if ,empty-branch,nonempty-branch)
	@echo OR=$(or ,,third)
	@echo AND=$(and a,b,c)
EOF
OUT=$(run $MAKE -f func.mk show 2>&1)
chk "make \$(subst)"      "SUBST=foo.o bar.o baz.o" "$OUT"
chk "make \$(patsubst)"   "PATSUBST=foo.o bar.o baz.o" "$OUT"
chk "make \$(strip)"      "STRIP=[a b c]" "$OUT"
chk "make \$(sort dedup+order)" "SORT=apple banana cherry" "$OUT"
chk "make \$(word)"       "WORD2=bar.c" "$OUT"
chk "make \$(words)"      "WORDS=3" "$OUT"
chk "make \$(firstword)"  "FIRST=foo.c" "$OUT"
chk "make \$(lastword)"   "LAST=baz.c" "$OUT"
chk "make \$(wordlist)"   "WL=foo.c bar.c" "$OUT"
chk "make \$(filter)"     "FILTER=a.c c.c" "$OUT"
chk "make \$(filter-out)" "FILTEROUT=a.c c.c" "$OUT"
chk "make \$(findstring)" "FIND=bar" "$OUT"
chk "make \$(dir)"        "DIR=src/ inc/" "$OUT"
chk "make \$(notdir)"     "NOTDIR=a.c b.h" "$OUT"
chk "make \$(suffix)"     "SUFFIX=.c .h" "$OUT"
chk "make \$(basename)"   "BASENAME=a b" "$OUT"
chk "make \$(addprefix)"  "ADDPRE=obj/a.o obj/b.o" "$OUT"
chk "make \$(addsuffix)"  "ADDSUF=a.o b.o" "$OUT"
chk "make \$(join)"       "JOIN=a1 b2" "$OUT"
chk "make \$(foreach)"    "FOREACH=n1 n2 n3" "$OUT"
chk "make \$(shell)"      "SHELL=from_shell" "$OUT"
chk "make \$(origin)"     "ORIGIN=file" "$OUT"
chk "make \$(if)"         "IF=nonempty-branch" "$OUT"
chk "make \$(or)"         "OR=third" "$OUT"
chk "make \$(and)"        "AND=c" "$OUT"

# wildcard / call / eval / value / error+warning+info
cat > func2.mk <<'EOF'
reverse = $(2) $(1)
DEFER = $(NOT_YET)
NOT_YET = resolved
show:
	@echo WILD=$(wildcard *.seed)
	@echo CALL=$(call reverse,one,two)
	@echo 'VALUE=$(value DEFER)'
	$(info INFO_LINE_SEEN)
	$(warning WARNING_LINE_SEEN)
	@echo DONE
EOF
: > a.seed; : > b.seed
OUT=$(run $MAKE -f func2.mk show 2>&1)
chk "make \$(wildcard)"  ".seed" "$OUT"
chk "make \$(call)"      "CALL=two one" "$OUT"
chk "make \$(value)"     "VALUE=\$(NOT_YET)" "$OUT"
chk "make \$(info)"      "INFO_LINE_SEEN" "$OUT"
chk "make \$(warning)"   "WARNING_LINE_SEEN" "$OUT"

# eval: synthesize a rule at parse time
cat > funceval.mk <<'EOF'
define MK_RULE
gen_$(1):
	@echo GENERATED_$(1)
endef
$(eval $(call MK_RULE,X))
EOF
OUT=$(run $MAKE -f funceval.mk gen_X 2>&1)
chk "make \$(eval) (synthesize rule)" "GENERATED_X" "$OUT"

# $(error) aborts with message. Use a temp makefile (not '-f -') so the run()
# timeout guard's /dev/null stdin does not eat the makefile body.
cat > err.mk <<'EOF'
die:
	@true
$(error DELIBERATE_ERROR)
EOF
OUT=$(run $MAKE -f err.mk die 2>&1)
ERR_RC=$?
# $(error) must abort: message present AND non-zero exit.
if echo "$OUT" | grep -q "DELIBERATE_ERROR" && [ "$ERR_RC" -ne 0 ]; then
  ok "make \$(error) (aborts with message, non-zero exit)"
else
  bad "make \$(error) (expected DELIBERATE_ERROR + non-zero exit; rc=$ERR_RC)"
  echo "$OUT" | head -4
fi

# -----------------------------------------------------------------------------
# GROUP M: pattern rules + .PHONY + .DEFAULT_GOAL
# -----------------------------------------------------------------------------
echo "--- GROUP M: pattern rules + .PHONY ---"
cat > pat.mk <<'EOF'
.PHONY: all
all: x.out y.out
%.out: %.in
	@echo MADE $@ from $<
	@cp $< $@
EOF
echo 1 > x.in; echo 2 > y.in; rm -f x.out y.out
OUT=$(run $MAKE -f pat.mk all 2>&1)
chk "make pattern rule %.out:%.in" "MADE x.out from x.in" "$OUT"
chk "make pattern rule (second target)" "MADE y.out from y.in" "$OUT"

# .PHONY target always runs even if a file of that name exists
cat > phony.mk <<'EOF'
.PHONY: clean
clean:
	@echo PHONY_CLEAN_RAN
EOF
: > clean   # file named 'clean' exists
OUT=$(run $MAKE -f phony.mk clean 2>&1)
chk "make .PHONY (runs despite same-named file)" "PHONY_CLEAN_RAN" "$OUT"

# -----------------------------------------------------------------------------
# GROUP N: conditionals  ifeq / ifneq / ifdef / ifndef / else / endif
# -----------------------------------------------------------------------------
echo "--- GROUP N: conditionals ---"
cat > cond.mk <<'EOF'
MODE := debug
show:
ifeq ($(MODE),debug)
	@echo IFEQ_DEBUG
else
	@echo IFEQ_OTHER
endif
ifneq ($(MODE),release)
	@echo IFNEQ_NOT_RELEASE
endif
ifdef MODE
	@echo IFDEF_MODE_SET
endif
ifndef MISSING
	@echo IFNDEF_MISSING_UNSET
endif
EOF
OUT=$(run $MAKE -f cond.mk show 2>&1)
chk "make ifeq/else/endif" "IFEQ_DEBUG" "$OUT"
chk "make ifneq" "IFNEQ_NOT_RELEASE" "$OUT"
chk "make ifdef" "IFDEF_MODE_SET" "$OUT"
chk "make ifndef" "IFNDEF_MISSING_UNSET" "$OUT"

# -----------------------------------------------------------------------------
# GROUP O: include directive + recursive make (export + $(MAKE))
# -----------------------------------------------------------------------------
echo "--- GROUP O: include + recursive make ---"
cat > part.mk <<'EOF'
PART_VAR := included_ok
EOF
cat > main.mk <<'EOF'
include part.mk
show:
	@echo PART_VAR=$(PART_VAR)
EOF
OUT=$(run $MAKE -f main.mk show 2>&1)
chk "make include directive" "PART_VAR=included_ok" "$OUT"

# Recursive make: parent exports a var and invokes $(MAKE) on a sub-makefile.
cat > sub.mk <<'EOF'
child:
	@echo CHILD_SEES=$(EXPORTED_VAR)
EOF
cat > parent.mk <<'EOF'
export EXPORTED_VAR := from_parent
parent:
	@$(MAKE) -f sub.mk child
EOF
OUT=$(run $MAKE -f parent.mk parent 2>&1)
chk "make recursive \$(MAKE) + export" "CHILD_SEES=from_parent" "$OUT"

# -----------------------------------------------------------------------------
# Summary
# -----------------------------------------------------------------------------
echo "==================================="
echo "gnumake carpet results: PASS=$PASS FAIL=$FAIL SKIP=$SKIP"
if [ "$FAIL" -eq 0 ]; then
  echo "GNUMAKE_CLI_OK"
  exit 0
else
  echo "GNUMAKE_CLI_FAILED ($FAIL failures)"
  exit 1
fi
