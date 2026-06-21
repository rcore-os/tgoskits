#!/bin/sh
# java-cli-core.sh — core (kernel-relevant) subset of the javac/java CLI carpet, for
# StarryOS starry runs under QEMU TCG. The FULL 68-check launcher surface lives in
# java-cli-carpet.sh and runs on the host golden; this curated ~28-check subset
# exercises the options whose behaviour actually depends on the kernel (process
# spawn / fork-exec, file I/O, exit-code propagation, env injection, single-file
# compile-and-run) plus the core compiler/runtime options — keeping each starry arch
# tractable under -Xint/TCG. Prints JAVA_CLI_OK iff every check passes.
#
# Heap: every JVM here passes an explicit -Xmx (java) / -J-Xmx (javac); a bare
# ergonomic-heap JVM hits "Too small maximum heap" on starry.
set -u
JAVAC="${JAVAC:-javac}"; JAVA="${JAVA:-java}"; RELOPT="${RELOPT:-}"
JV="-Xmx256m -Xms32m"
W="$(mktemp -d "${TMPDIR:-/tmp}/jcore.XXXXXX")"; OK=1
ok(){ printf '  ok %s %s\n' "$1" "${2:-}"; }
bad(){ printf '  FAIL %s %s\n' "$1" "${2:-}"; OK=0; }
chk(){ if [ "$2" = 0 ]; then ok "$1" "$3"; else bad "$1" "$3"; fi; }
JC(){ $JAVAC -J-Xmx256m $RELOPT "$@"; }
# strip noise (JAVA_TOOL_OPTIONS banner / HotSpot VM warnings / deprecation) before parsing
# version strings. "VM warning:" covers arch quirks like riscv64's
# "OpenJDK 64-Bit Server VM warning: RVC is not supported on this CPU".
nostderr(){ grep -avE 'Picked up JAVA_TOOL_OPTIONS|VM warning:|^Warning:|deprecated'; }
mkdir -p "$W/out"
cat > "$W/Hello.java" <<'EOF'
public class Hello {
  public static void main(String[] a){
    boolean ae=false; assert (ae=true);
    System.out.println("HELLO "+(a.length>0?a[0]:"world"));
    System.out.println("ASSERT "+ae);
    System.out.println("PROP "+System.getProperty("carpet.k","unset"));
    System.out.println("ENVOPT "+System.getProperty("carpet.env","unset"));
  }
}
EOF

# ---- javac core options ----
V="$($JAVAC -version 2>&1 | nostderr)"; case "$V" in javac*) ok javac_version "$V";; *) bad javac_version "$V";; esac
$JAVAC --help >"$W/h" 2>&1; grep -q -- '--class-path' "$W/h"; chk javac_help $? ""
JC -d "$W/out" "$W/Hello.java" 2>"$W/e"; [ -f "$W/out/Hello.class" ]; chk javac_d $? "$(cat "$W/e")"
JC -cp "$W/out" -d "$W/o2" "$W/Hello.java" 2>/dev/null; chk javac_cp $? ""
JC --release 17 -d "$W/o3" "$W/Hello.java" 2>/dev/null; chk javac_release $? ""
JC -encoding UTF-8 -d "$W/o4" "$W/Hello.java" 2>/dev/null; chk javac_encoding $? ""
JC -g -d "$W/o5" "$W/Hello.java" 2>/dev/null; chk javac_g $? ""
JC -Xlint:all -d "$W/o6" "$W/Hello.java" 2>/dev/null; chk javac_Xlint $? ""

# ---- java core + kernel-relevant options ----
V="$($JAVA $JV -version 2>&1 | nostderr | head -1)"; case "$V" in *version*) ok java_version "$V";; *) bad java_version "$V";; esac
$JAVA $JV --help >"$W/jh" 2>&1; grep -q -- '--class-path' "$W/jh"; chk java_help $? ""
$JAVA $JV -X >"$W/jx" 2>&1; grep -q -- '-Xmx' "$W/jx"; chk java_X_nonstd $? ""
$JAVA $JV -cp "$W/out" Hello CLI 2>/dev/null | grep -q 'HELLO CLI'; chk java_cp $? ""
$JAVA $JV -cp "$W/out" -Dcarpet.k=SET Hello 2>/dev/null | grep -q 'PROP SET'; chk java_Dprop $? ""
$JAVA $JV -cp "$W/out" -ea Hello 2>/dev/null | grep -q 'ASSERT true'; chk java_ea $? ""
$JAVA $JV -cp "$W/out" Hello 2>/dev/null | grep -q 'ASSERT false'; chk java_da_default $? ""
# -jar (Main-Class manifest; exercises jar tool + launcher)
( cd "$W/out" && printf 'Main-Class: Hello\n' > mf && jar cfm "$W/a.jar" mf Hello.class 2>/dev/null )
if [ -f "$W/a.jar" ]; then $JAVA $JV -jar "$W/a.jar" J 2>/dev/null | grep -q 'HELLO J'; chk java_jar $? ""; else ok java_jar "(skip: no jar tool)"; fi
# --source : single-file source-code launcher (kernel fork/exec + in-memory compile)
$JAVA $JV --source 17 "$W/Hello.java" SRC 2>/dev/null | grep -q 'HELLO SRC'; chk java_source_singlefile $? ""
# --dry-run : load+link, do not run main
$JAVA $JV -cp "$W/out" --dry-run Hello 2>/dev/null | grep -q HELLO; [ $? -ne 0 ]; chk java_dry_run $? ""
$JAVA $JV --list-modules 2>/dev/null | grep -q java.base; chk java_list_modules $? ""
$JAVA $JV --describe-module java.base 2>/dev/null | grep -q 'exports java.lang'; chk java_describe_module $? ""
$JAVA $JV -cp "$W/out" -verbose:class Hello >"$W/vc" 2>&1; grep -qi 'load' "$W/vc"; chk java_verbose_class $? ""
$JAVA $JV -cp "$W/out" Hello 2>/dev/null | grep -q HELLO; chk java_Xmx $? "(explicit -Xmx heap)"
$JAVA $JV -XshowSettings:properties -version >"$W/ss" 2>&1; grep -qi 'java.version' "$W/ss"; chk java_XshowSettings $? ""
# exit-code propagation (kernel)
cat > "$W/Exit.java" <<'EOF'
public class Exit { public static void main(String[] a){ System.exit(7); } }
EOF
JC -d "$W/ex" "$W/Exit.java" 2>/dev/null
$JAVA $JV -cp "$W/ex" Exit; [ $? -eq 7 ]; chk java_exit_code $? ""
# env JAVA_TOOL_OPTIONS is honored: the JVM announces "Picked up JAVA_TOOL_OPTIONS:
# <opts>" on startup when the env var is set. (We assert the env is consumed; the
# launcher prints this whenever JTO is present — reliable across host & starry.)
( export JAVA_TOOL_OPTIONS="-Xint -Xmx256m"; $JAVA -version ) 2>&1 | grep -q 'Picked up JAVA_TOOL_OPTIONS'; chk java_JAVA_TOOL_OPTIONS $? ""
# env CLASSPATH
( CLASSPATH="$W/out" $JAVA $JV Hello 2>/dev/null | grep -q HELLO ); chk java_env_classpath $? ""

rm -rf "$W"
if [ "$OK" = 1 ]; then echo "JAVA_CLI_OK"; exit 0; else echo "JAVA_CLI_FAIL"; exit 1; fi
