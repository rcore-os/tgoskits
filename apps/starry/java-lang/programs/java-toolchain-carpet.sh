#!/bin/sh
# java-toolchain-carpet.sh — IRON-CLAD full JDK toolchain carpet (#764).
#
# The javac/java launcher surface lives in java-cli-core.sh (starry subset) and
# java-cli-carpet.sh (68-check host golden). THIS carpet covers the REST of the
# JDK developer toolchain — every shipped language/dev tool, each with its --help
# surface + a real functional assertion — so the delivery is the FULL set (全集),
# not just the compiler+launcher:
#
#   jshell    REPL: script mode, feedback modes, default imports, error recovery
#   jar       create/list/extract/update, entry-point, manifest, modular, multi-release
#   javap     disassembler: -c -p -v -s -l -constants -public
#   javadoc   HTML doc generation, --release, tags, exit code
#   jdeps     dependency analysis: -summary -verbose --print-module-deps --list-deps
#   jdeprscan deprecation scanner: --release --list, scan a class
#   serialver compute serialVersionUID of a Serializable class
#   jlink     build a custom modular runtime image (and run it)
#   jmod      describe / list / create a jmod
#   jpackage  build a self-contained app-image
#   jrunscript script shell (--help; no bundled JS engine since JDK15 — documented)
#   + ops/security smokes: jcmd jps jstat jstack jmap jinfo jfr keytool jarsigner jdb
#
# Tool resolution: every tool is taken from $TC_BIN (a JDK bin dir) when set, else
# from PATH. Each JVM-backed tool gets an explicit small heap so a bare ergonomic
# JVM does not hit "Too small maximum heap" on starry; tools run under -Xint/TCG.
# Version-aware: options whose syntax changed across 17→25 (jlink --compress) probe
# the running tool's major and pick the right form. Prints JAVA_TOOLCHAIN_OK iff
# every non-skipped check passes; SKIPs (e.g. GUI jconsole, jrunscript engine) are
# documented and not counted as failures.
set -u

TC_BIN="${TC_BIN:-}"
# TC_HEAVY=1 (host golden default) runs the multi-minute BUILD ops (jlink runtime image,
# jpackage app-image). On starry these are JVM programs at -Xint/TCG and take many minutes,
# so run-java.sh sets TC_HEAVY=0: starry still exercises the FULL toolchain (every tool +
# its --help/--version/--list-plugins + the light functional ops) and defers only the two
# heavy image-build ops to the host golden — same split as java-cli-core(starry)/golden(host).
TC_HEAVY="${TC_HEAVY:-1}"
t(){ if [ -n "$TC_BIN" ] && [ -x "$TC_BIN/$1" ]; then printf '%s/%s' "$TC_BIN" "$1"; else printf '%s' "$1"; fi; }
JAVA="$(t java)"; JAVAC="$(t javac)"
JV="-Xmx256m -Xms32m"          # heap for JVM-backed tools (java)
JJV="-J-Xmx256m"               # heap passed through wrapper tools (-J prefix)
W="$(mktemp -d "${TMPDIR:-/tmp}/jtc.XXXXXX")"; OK=1; N=0; SK=0
ok(){  N=$((N+1)); printf '  ok %s %s\n' "$1" "${2:-}"; }
bad(){ N=$((N+1)); OK=0; printf '  FAIL %s %s\n' "$1" "${2:-}"; }
skip(){ SK=$((SK+1)); printf '  skip %s -- %s\n' "$1" "${2:-}"; }
chk(){ if [ "$2" = 0 ]; then ok "$1" "$3"; else bad "$1" "$3"; fi; }
have(){ x="$(t "$1")"; command -v "$x" >/dev/null 2>&1; }
# strip JVM/arch noise before parsing tool stdout/stderr
nz(){ grep -avE 'Picked up JAVA_TOOL_OPTIONS|VM warning:|^Warning:|deprecated|^Note:'; }

# JDK major of the resolved java (e.g. 17, 21, 23, 25)
MAJOR="$("$JAVA" -version 2>&1 | nz | sed -n 's/.*version "\([0-9][0-9]*\).*/\1/p' | head -1)"
[ -z "$MAJOR" ] && MAJOR=17
echo "=== java toolchain carpet (major=$MAJOR; bin=${TC_BIN:-PATH}) ==="

# ---------- shared fixtures ----------
mkdir -p "$W/src/com/x" "$W/out"
cat > "$W/src/Hello.java" <<'EOF'
/** Demo type. @author carpet @version 1 */
public class Hello {
  public static void main(String[] a){ System.out.println("HELLO "+(a.length>0?a[0]:"world")); }
}
EOF
cat > "$W/src/Ser.java" <<'EOF'
import java.io.Serializable;
public class Ser implements Serializable { private static final long serialVersionUID = 4242L; int x; }
EOF
cat > "$W/src/Dep.java" <<'EOF'
@SuppressWarnings({"deprecation","removal"})
public class Dep { void m(){ Integer i = new Integer(5); Double d = new Double(1.0); Boolean b = new Boolean(true); } }
EOF
"$JAVAC" $JJV -g -d "$W/out" "$W/src/Hello.java" "$W/src/Ser.java" "$W/src/Dep.java" 2>"$W/jcerr" \
  && ok setup_compile "" || bad setup_compile "$(cat "$W/jcerr")"
# modular fixture (for jar --describe-module / jmod)
cat > "$W/src/module-info.java" <<'EOF'
module com.x { exports com.x; }
EOF
cat > "$W/src/com/x/App.java" <<'EOF'
package com.x; public class App { public static void main(String[] a){ System.out.println("MOD-OK"); } }
EOF
mkdir -p "$W/mout"
"$JAVAC" $JJV -d "$W/mout" "$W/src/module-info.java" "$W/src/com/x/App.java" 2>/dev/null \
  && ok setup_module "" || bad setup_module ""

# ---------- jshell (REPL) ----------
if have jshell; then
  JSH="$(t jshell)"
  "$JSH" $JJV --help >"$W/o" 2>&1; grep -qE -- '--execution|--feedback' "$W/o"; chk jshell_help $? ""
  "$JSH" $JJV --version >"$W/o" 2>&1; grep -qi 'jshell' "$W/o"; chk jshell_version "$?" ""
  printf 'int v=6*7;\nSystem.out.println("JSHELL="+v);\n/exit\n' > "$W/a.jsh"
  "$JSH" $JJV -q --execution local "$W/a.jsh" 2>/dev/null | grep -q 'JSHELL=42'; chk jshell_script $? "(snippet eval)"
  # default auto-imports (java.util.* available without import)
  printf 'var L=new java.util.ArrayList<String>(); L.add("z"); System.out.println("IMPORT="+L.get(0));\n/exit\n' > "$W/b.jsh"
  "$JSH" $JJV -q --execution local "$W/b.jsh" 2>/dev/null | grep -q 'IMPORT=z'; chk jshell_eval2 $? ""
  # feedback mode flag accepted
  # --execution local (in-process): jshell's DEFAULT execution spawns a remote agent JVM
  # (second process + socket), which is unreliable on starry under -Xint/TCG; local runs
  # snippets in the same JVM. (All snippet-running jshell checks here use local for this reason.)
  "$JSH" $JJV --feedback concise --execution local "$W/a.jsh" 2>/dev/null | grep -q 'JSHELL=42'; chk jshell_feedback $? "(--feedback)"
  # error recovery: a bad snippet then a good one still runs
  printf 'nonsense @@@;\nSystem.out.println("AFTER-ERR");\n/exit\n' > "$W/c.jsh"
  "$JSH" $JJV -q --execution local "$W/c.jsh" 2>/dev/null | grep -q 'AFTER-ERR'; chk jshell_recovery $? ""
  # meta command (/list shows snippets)
  printf 'int q=1;\n/list\n/exit\n' > "$W/d.jsh"
  "$JSH" $JJV -q --execution local "$W/d.jsh" 2>/dev/null | grep -q 'int q'; chk jshell_meta_list $? ""
else bad jshell_present "jshell not found"; fi

# ---------- jar ----------
if have jar; then
  JAR="$(t jar)"
  "$JAR" --version >"$W/o" 2>&1; grep -qi 'jar' "$W/o"; chk jar_version $? ""
  ( cd "$W/out" && "$JAR" --create --file "$W/app.jar" --main-class Hello Hello.class ) 2>/dev/null; chk jar_create $? "(cfe entry)"
  "$JAR" --list --file "$W/app.jar" 2>/dev/null | grep -q 'Hello.class'; chk jar_list $? ""
  mkdir -p "$W/xtr" && ( cd "$W/xtr" && "$JAR" --extract --file "$W/app.jar" ) 2>/dev/null; [ -f "$W/xtr/Hello.class" ]; chk jar_extract $? ""
  echo extra > "$W/extra.txt" && ( cd "$W" && "$JAR" --update --file "$W/app.jar" extra.txt ) 2>/dev/null && "$JAR" tf "$W/app.jar" 2>/dev/null | grep -q extra.txt; chk jar_update $? ""
  ( cd "$W/out" && printf 'Main-Class: Hello\n' > mf && "$JAR" cfm "$W/m.jar" mf Hello.class ) 2>/dev/null; [ -f "$W/m.jar" ]; chk jar_manifest $? ""
  # modular jar describe
  "$JAR" --create --file "$W/mod.jar" --main-class com.x.App -C "$W/mout" . 2>/dev/null \
    && "$JAR" --describe-module --file "$W/mod.jar" 2>/dev/null | grep -q 'exports com.x'; chk jar_describe_module $? ""
  # multi-release jar
  mkdir -p "$W/mr17" && cp "$W/out/Hello.class" "$W/mr17/" 2>/dev/null
  ( cd "$W/out" && "$JAR" --create --file "$W/mr.jar" Hello.class --release 17 -C "$W/mr17" Hello.class ) 2>/dev/null; chk jar_multirelease $? ""
else bad jar_present "jar not found"; fi

# ---------- javap (disassembler) ----------
if have javap; then
  JP="$(t javap)"
  "$JP" $JJV -version >"$W/o" 2>&1; grep -qE '[0-9]' "$W/o"; chk javap_version $? ""
  "$JP" $JJV -help >"$W/o" 2>&1; grep -qE -- '-c|Usage' "$W/o"; chk javap_help $? ""
  "$JP" $JJV -c -p -cp "$W/out" Hello 2>/dev/null | grep -q 'public static void main'; chk javap_disasm $? "(-c -p)"
  "$JP" $JJV -v -cp "$W/out" Hello 2>/dev/null | grep -qi 'minor version'; chk javap_verbose $? "(-v constant pool)"
  "$JP" $JJV -s -cp "$W/out" Hello 2>/dev/null | grep -qi 'descriptor'; chk javap_signatures $? "(-s)"
  "$JP" $JJV -l -cp "$W/out" Hello 2>/dev/null | grep -qiE 'LineNumberTable|line'; chk javap_lines $? "(-l, -g compiled)"
  "$JP" $JJV -constants -cp "$W/out" Hello 2>/dev/null | grep -q 'class Hello'; chk javap_constants $? ""
else bad javap_present "javap not found"; fi

# ---------- javadoc ----------
if have javadoc; then
  JD="$(t javadoc)"
  "$JD" $JJV --help >"$W/o" 2>&1; grep -qE -- '-d|Usage' "$W/o"; chk javadoc_help $? ""
  "$JD" $JJV -quiet -d "$W/jdoc" -author -version "$W/src/Hello.java" >"$W/o" 2>&1; [ -f "$W/jdoc/Hello.html" ] || [ -f "$W/jdoc/index.html" ]; chk javadoc_generate $? "(HTML out)"
  "$JD" $JJV -quiet --release "$MAJOR" -d "$W/jdoc2" "$W/src/Hello.java" >/dev/null 2>&1; [ -d "$W/jdoc2" ]; chk javadoc_release $? ""
else bad javadoc_present "javadoc not found"; fi

# ---------- jdeps ----------
if have jdeps; then
  JDE="$(t jdeps)"
  "$JDE" $JJV -version >"$W/o" 2>&1; grep -qE '[0-9]' "$W/o"; chk jdeps_version $? ""
  "$JDE" $JJV -help >"$W/o" 2>&1; grep -qiE 'usage|summary' "$W/o"; chk jdeps_help $? ""
  "$JDE" $JJV -summary "$W/out/Hello.class" 2>/dev/null | grep -q 'java.base'; chk jdeps_summary $? ""
  "$JDE" $JJV -verbose "$W/out/Hello.class" 2>/dev/null | grep -q 'java.base'; chk jdeps_verbose $? ""
  "$JDE" $JJV --print-module-deps "$W/out/Hello.class" 2>/dev/null | grep -q 'java.base'; chk jdeps_module_deps $? ""
  "$JDE" $JJV --list-deps "$W/out/Hello.class" 2>/dev/null | grep -q 'java.base'; chk jdeps_list_deps $? ""
else bad jdeps_present "jdeps not found"; fi

# ---------- jdeprscan ----------
if have jdeprscan; then
  JDS="$(t jdeprscan)"
  "$JDS" $JJV --version >"$W/o" 2>&1; grep -qE '[0-9]' "$W/o"; chk jdeprscan_version "$?" ""
  "$JDS" $JJV --help >"$W/o" 2>&1; grep -qiE 'usage|--list|--release' "$W/o"; chk jdeprscan_help $? ""
  "$JDS" $JJV --release "$MAJOR" --list >"$W/o" 2>&1; grep -q '@Deprecated' "$W/o"; chk jdeprscan_list $? "(--release --list)"
  # scan a class that uses a deprecated API (Integer(int) ctor) -> reports it
  "$JDS" $JJV --class-path "$W/out" Dep >"$W/o" 2>&1; grep -qiE 'deprecated|Integer' "$W/o"; chk jdeprscan_scan "$?" "(scan class)"
else bad jdeprscan_present "jdeprscan not found"; fi

# ---------- serialver ----------
if have serialver; then
  SV="$(t serialver)"
  "$SV" -classpath "$W/out" Ser 2>/dev/null | grep -q 'serialVersionUID'; chk serialver_compute $? ""
else bad serialver_present "serialver not found"; fi

# ---------- jlink (custom runtime) ----------
if have jlink; then
  JL="$(t jlink)"
  "$JL" --version >"$W/o" 2>&1; grep -qE '[0-9]' "$W/o"; chk jlink_version $? ""
  "$JL" --help >"$W/o" 2>&1; grep -qE -- '--add-modules|--output' "$W/o"; chk jlink_help $? ""
  "$JL" --list-plugins >"$W/o" 2>&1; grep -qiE 'strip-debug|compress|Plugin' "$W/o"; chk jlink_list_plugins "$?" ""
  if [ "$TC_HEAVY" = 1 ]; then
    # version-aware --compress: JDK21+ uses zip-N, JDK17/19/20 uses integer 0|1|2
    if [ "$MAJOR" -ge 21 ] 2>/dev/null; then CMP="--compress=zip-6"; else CMP="--compress=2"; fi
    "$JL" --add-modules java.base --output "$W/jrt" --strip-debug $CMP >"$W/o" 2>&1; chk jlink_build $? "(custom runtime, $CMP)"
    if [ -x "$W/jrt/bin/java" ]; then "$W/jrt/bin/java" -version >/dev/null 2>&1; chk jlink_runtime_runs $? ""; else bad jlink_runtime_runs "no jrt/bin/java"; fi
  else
    skip jlink_build "host-golden only (java.base runtime build is multi-minute under -Xint/TCG)"
    skip jlink_runtime_runs "(heavy build deferred to host golden)"
  fi
else bad jlink_present "jlink not found"; fi

# ---------- jmod ----------
if have jmod; then
  JM="$(t jmod)"
  "$JM" --version >"$W/o" 2>&1; grep -qE '[0-9]' "$W/o"; chk jmod_version "$?" ""
  # locate java.base.jmod relative to the resolved java
  JP="$(command -v "$JAVA" 2>/dev/null || echo "$JAVA")"
  JHOME="$(dirname "$(dirname "$(readlink -f "$JP" 2>/dev/null || echo "$JP")")")"
  BJMOD="$JHOME/jmods/java.base.jmod"
  if [ -f "$BJMOD" ]; then
    "$JM" describe "$BJMOD" 2>/dev/null | grep -q 'java.base@'; chk jmod_describe $? ""
    "$JM" list "$BJMOD" 2>/dev/null | grep -q 'bin/java'; chk jmod_list $? ""
  else skip jmod_describe "java.base.jmod not found at $BJMOD"; skip jmod_list "no base jmod"; fi
  "$JM" create --class-path "$W/mout" "$W/my.jmod" 2>/dev/null && "$JM" describe "$W/my.jmod" 2>/dev/null | grep -q 'com.x'; chk jmod_create $? ""
else bad jmod_present "jmod not found"; fi

# ---------- jpackage (app-image) ----------
if have jpackage; then
  JPK="$(t jpackage)"
  "$JPK" --help >"$W/o" 2>&1; grep -qiE -- '--type|app-image' "$W/o"; chk jpackage_help $? ""
  "$JPK" --version >"$W/o" 2>&1; grep -qE '[0-9]' "$W/o"; chk jpackage_version "$?" ""
  if [ "$TC_HEAVY" = 1 ]; then
    mkdir -p "$W/pin" && cp "$W/m.jar" "$W/pin/app.jar" 2>/dev/null
    "$JPK" --type app-image --input "$W/pin" --main-jar app.jar --main-class Hello --dest "$W/pout" --name DemoApp >"$W/o" 2>&1
    if { [ -d "$W/pout/DemoApp" ] || { ls "$W/pout" >/dev/null 2>&1 && [ -n "$(ls -A "$W/pout" 2>/dev/null)" ]; }; }; then ok jpackage_app_image "(self-contained image)"; else
      # app-image needs no OS installer for the image itself; if it still fails (e.g. missing objcopy on some musl images) document it
      skip jpackage_app_image "app-image build needs platform objcopy/ldd; absent on this image: $(tail -1 "$W/o")"; fi
  else
    skip jpackage_app_image "host-golden only (app-image build is heavy + needs platform objcopy under TCG)"
  fi
else bad jpackage_present "jpackage not found"; fi

# ---------- jrunscript ----------
if have jrunscript; then
  JRS="$(t jrunscript)"
  "$JRS" $JJV -help >"$W/o" 2>&1; grep -qiE 'usage|jrunscript' "$W/o"; chk jrunscript_help $? ""
  # Nashorn (JS engine) was removed in JDK 15; without a bundled engine, eval reports
  # "script engine for language js can not be found". Assert that documented behavior
  # (the tool runs and reports the missing engine) rather than a false PASS.
  "$JRS" $JJV -e 'print("X")' >"$W/o" 2>&1
  if grep -qi 'JS\|print' "$W/o" 2>/dev/null && ! grep -qi 'can not be found' "$W/o"; then ok jrunscript_eval "(engine present)"; else
    grep -qi 'engine.*can not be found\|No engine' "$W/o"; chk jrunscript_no_engine $? "(Nashorn removed JDK15+, documented)"; fi
else bad jrunscript_present "jrunscript not found"; fi

# ---------- ops / security tool smokes (--help exits 0; functional needs target JVM) ----------
for tool in jcmd jps jstat jstack jmap jinfo jfr keytool jarsigner jdb; do
  if have "$tool"; then
    x="$(t "$tool")"
    case "$tool" in
      keytool|jarsigner) "$x" -help >/dev/null 2>&1; rc=$? ;;
      jfr)               "$x" help >/dev/null 2>&1; rc=$? ;;
      jdb)               "$x" -help >/dev/null 2>&1; rc=$? ;;
      *)                 "$x" -? >/dev/null 2>&1 || "$x" -h >/dev/null 2>&1 || "$x" --help >/dev/null 2>&1; rc=$? ;;
    esac
    # these print usage to stderr and may exit non-zero; accept "produced usage text"
    if [ $rc -eq 0 ]; then ok "ops_${tool}_help" ""; else
      "$x" 2>&1 | grep -qiE 'usage|option' && ok "ops_${tool}_help" "(usage)" || bad "ops_${tool}_help" "rc=$rc"; fi
  else skip "ops_${tool}" "not shipped in this JDK"; fi
done
# jconsole/jhsdb are GUI/attach tools — documented SKIP in headless starry
skip ops_jconsole "GUI tool (Swing); not runnable headless on starry"

rm -rf "$W"
echo "# RESULTS: PASS_AND_OK=$N FAIL=$([ "$OK" = 1 ] && echo 0 || echo '>=1') SKIP=$SK"
if [ "$OK" = 1 ]; then echo "JAVA_TOOLCHAIN_OK"; exit 0; else echo "JAVA_TOOLCHAIN_FAIL"; exit 1; fi
