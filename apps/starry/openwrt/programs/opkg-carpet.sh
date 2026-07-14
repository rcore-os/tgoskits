#!/bin/sh
# opkg-carpet.sh - doc-grounded carpet for OpenWrt opkg (the .ipk package manager).
#
# Ground truth = `opkg` usage tree: package-manip (update/upgrade/install/configure/remove/
# flag), informational (list/list-installed/list-upgradable/files/search/find/info/status/
# download/compare-versions/print-architecture/depends/whatdepends/whatprovides ...) and the
# option surface (-A -V --offline-root/-o --conf/-f --add-arch --force-*). The carpet is fully
# hermetic and offline: it builds its own synthetic .ipk feed at runtime (tar.gz outer format,
# no network, no ar - busybox tar/gzip only) into an offline-root and drives opkg against it.
#
# opkg binary: $1, else $OPKG_BIN, else `opkg-cl`/`opkg` on PATH. Prints "OPKG CARPET OK <n>"
# iff every assertion passed AND the count equals the pinned total.
set -u
OPKG="${1:-${OPKG_BIN:-}}"
[ -n "$OPKG" ] || { OPKG="$(command -v opkg-cl 2>/dev/null || command -v opkg 2>/dev/null)"; }
[ -n "$OPKG" ] && command -v "$OPKG" >/dev/null 2>&1 || { echo "opkg not found"; echo "OPKG CARPET FAIL"; exit 1; }

ARCH="${OPKG_ARCH:-$(uname -m 2>/dev/null || echo x86_64)}"
case "$ARCH" in x86_64|aarch64|riscv64|loongarch64) : ;; *) ARCH=x86_64 ;; esac

R="${OPKG_WORK:-/tmp/opkg-carpet.$$}"
FEED="$R/feed"; ROOT="$R/root"; BUILD="$R/build"
rm -rf "$R"; mkdir -p "$FEED" "$BUILD" "$ROOT/var/lock" "$ROOT/usr/lib/opkg/lists" "$ROOT/usr/lib/opkg/info"

cat > "$R/opkg.conf" <<EOF
arch $ARCH 10
arch all 1
src/gz local file://$FEED
dest root /
lists_dir ext /usr/lib/opkg/lists
EOF
O() { "$OPKG" -f "$R/opkg.conf" -o "$ROOT" "$@"; }

PASS=0; FAIL=0
pass() { PASS=$((PASS+1)); }
fail() { FAIL=$((FAIL+1)); echo "FAIL: $*"; }
eq() { if [ "$2" = "$3" ]; then pass; else fail "$1 | got=[$2] want=[$3]"; fi; }
ok() { d="$1"; shift; if "$@" >/dev/null 2>&1; then pass; else fail "$d (expected success)"; fi; }
no() { d="$1"; shift; if "$@" >/dev/null 2>&1; then fail "$d (expected failure)"; else pass; fi; }
has(){ case "$2" in *"$3"*) pass;; *) fail "$1 | [$2] lacks [$3]";; esac; }

# ---------------------------------------------------------------- .ipk builder (tar.gz format)
# mkipk <name> <version> <arch> <depends> <file-under-usr/bin> <payload>
mkipk() {
	nm="$1"; ver="$2"; parch="$3"; deps="$4"; binname="$5"; payload="$6"
	pd="$BUILD/$nm"; rm -rf "$pd"; mkdir -p "$pd/control" "$pd/data/usr/bin"
	printf '#!/bin/sh\n%s\n' "$payload" > "$pd/data/usr/bin/$binname"; chmod +x "$pd/data/usr/bin/$binname"
	{
		echo "Package: $nm"; echo "Version: $ver"; echo "Architecture: $parch"
		echo "Maintainer: StarryWRT"; [ -n "$deps" ] && echo "Depends: $deps"
		echo "Description: synthetic $nm"
	} > "$pd/control/control"
	( cd "$pd/control" && tar -czf ../control.tar.gz . )
	( cd "$pd/data"    && tar -czf ../data.tar.gz . )
	printf '2.0\n' > "$pd/debian-binary"
	( cd "$pd" && tar -czf "$FEED/${nm}_${ver}_${parch}.ipk" ./debian-binary ./control.tar.gz ./data.tar.gz )
}
# regen Packages index from every .ipk currently in the feed
mkindex() {
	: > "$FEED/Packages"
	for ipk in "$FEED"/*.ipk; do
		[ -e "$ipk" ] || continue
		base="$(basename "$ipk")"; sz="$(wc -c < "$ipk" | tr -d ' ')"; sha="$(sha256sum "$ipk" | cut -d' ' -f1)"
		# recover control from the ipk to echo Package/Version/Architecture/Depends verbatim
		tmp="$R/idxtmp"; rm -rf "$tmp"; mkdir -p "$tmp"
		# extract all members (no explicit member name - portable across GNU and busybox tar)
		tar -xzf "$ipk" -C "$tmp" 2>/dev/null; tar -xzf "$tmp/control.tar.gz" -C "$tmp" 2>/dev/null
		grep -E '^(Package|Version|Architecture|Depends|Description):' "$tmp/control" >> "$FEED/Packages"
		echo "Filename: $base" >> "$FEED/Packages"
		echo "Size: $sz" >> "$FEED/Packages"
		echo "SHA256sum: $sha" >> "$FEED/Packages"
		echo "" >> "$FEED/Packages"
	done
	gzip -kf "$FEED/Packages"
}

# ---------------------------------------------------------------- self-id + pure ops (no feed)
has "usage on bare"        "$(O 2>&1)"                      "sub-command"
ok  "print-architecture"   sh -c "\"$OPKG\" -f \"$R/opkg.conf\" -o \"$ROOT\" print-architecture | grep -q '$ARCH 10'"
# compare-versions across every documented operator
ok  "cmp <<"   O compare-versions 1.0    '<<' 2.0
ok  "cmp <="   O compare-versions 2.0    '<=' 2.0
ok  "cmp ="    O compare-versions 1.5    '='  1.5
ok  "cmp >="   O compare-versions 2.0    '>=' 2.0
ok  "cmp >>"   O compare-versions 2.0    '>>' 1.0
no  "cmp << false" O compare-versions 2.0 '<<' 1.0
ok  "cmp epoch/rev" O compare-versions 1.0-1 '<<' 1.0-2

# ---------------------------------------------------------------- build feed + update/list
mkipk libhello 1.0.0 all      ""          libhello   "true"
mkipk hello    1.0.0 "$ARCH"  "libhello"  hello      'echo hi-starrywrt'
mkipk world    2.1.0 "$ARCH"  ""          world      'echo world'
mkindex
ok  "update (file feed)"   O update
LIST="$(O list 2>&1)"
has "list has hello"       "$LIST"  "hello - 1.0.0"
has "list has world"       "$LIST"  "world - 2.1.0"
has "list has libhello"    "$LIST"  "libhello - 1.0.0"
INFO="$(O info hello 2>&1)"
has "info Package"         "$INFO"  "Package: hello"
has "info Depends"         "$INFO"  "libhello"

# ---------------------------------------------------------------- install + dependency pull-in
ok  "install hello (+dep)" O install hello
has "hello file runs"      "$("$ROOT/usr/bin/hello" 2>&1)"   "hi-starrywrt"
[ -x "$ROOT/usr/bin/libhello" ] && pass || fail "dependency libhello not pulled in"
LI="$(O list-installed 2>&1)"
has "list-installed hello" "$LI"  "hello - 1.0.0"
has "list-installed dep"   "$LI"  "libhello - 1.0.0"
ST="$(O status hello 2>&1)"
has "status installed"     "$ST"  "install user installed"
FILES="$(O files hello 2>&1)"
has "files lists binary"   "$FILES"  "/usr/bin/hello"

# ---------------------------------------------------------------- dependency queries
has "depends of hello"     "$(O depends hello 2>&1)"       "libhello"
has "whatdepends libhello" "$(O whatdepends libhello 2>&1)" "hello"
ok  "whatprovides"         O whatprovides hello

# ---------------------------------------------------------------- flag + protected remove
ok  "flag hold dep"        O flag hold libhello
has "status shows hold"    "$(O status libhello 2>&1)"     "hold"
ok  "flag ok dep"          O flag ok libhello
ok  "install standalone"   O install world
ok  "remove world"         O remove world
[ -x "$ROOT/usr/bin/world" ] && fail "world file survived remove" || pass
# removing a depended-upon package without force is refused; --force-depends allows it
no  "remove dep refused"   O remove libhello
ok  "remove --force-depends" O --force-depends remove libhello

# ---------------------------------------------------------------- reinstall + upgrade path
ok  "force-reinstall"      O --force-reinstall install hello
# publish hello 1.2.0, re-index, list-upgradable + upgrade
mkipk hello 1.2.0 "$ARCH" "libhello" hello 'echo hi-v2'
mkindex
ok  "update v2"            O update
has "list-upgradable"      "$(O list-upgradable 2>&1)"     "hello"
ok  "upgrade hello"        O upgrade hello
has "upgraded to 1.2.0"    "$(O status hello 2>&1)"        "1.2.0"
has "v2 payload"           "$("$ROOT/usr/bin/hello" 2>&1)" "hi-v2"

# ---------------------------------------------------------------- error / option surface
no  "install missing pkg"  O install no-such-package-xyz
# -A queries all (not just installed); find matches name/description
has "find by name"         "$(O find world 2>&1)"          "world"
# --add-arch registers an arch (accepted)
ok  "--add-arch accepted"  O --add-arch mips:5 print-architecture

# ---------------------------------------------------------------- verdict
rm -rf "$R"
EXPECTED=42
TOTAL=$((PASS+FAIL))
echo "opkg: PASS=$PASS FAIL=$FAIL TOTAL=$TOTAL EXPECTED=$EXPECTED"
if [ "$FAIL" -eq 0 ] && [ "$TOTAL" -eq "$EXPECTED" ]; then
	echo "OPKG CARPET OK $PASS"
else
	echo "OPKG CARPET FAIL"
	exit 1
fi
