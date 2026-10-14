#!/bin/sh
# Prepare the Orange Pi 5 Plus guest root images for the local APT hello scenario.
#
# Run on the BOARD NATIVE Linux as root, with the guests stopped and after both images
# have been backed up and `e2fsck -f -y` has been run on them:
#
#   sh test-suit/axvisor/normal/board-orangepi-5-plus/triple-guest-functional/prepare-guest-apt-repo.sh \
#     /guest/rootfs-aarch64-orangepi-jammy-0.img \
#     /guest/rootfs-aarch64-orangepi-jammy-1.img
#
# Without arguments the two standard guest images above are prepared. The script is
# fail-closed and idempotent: it refuses a loop-attached or unclean image, mounts it
# read-write under a cleanup trap, verifies the single cached `hello` .deb with the
# board's dpkg-deb, and builds an isolated flat `file:` repository plus APT
# configuration under /usr/local/share/axvisor-guest-apt before warming the guest
# caches with `apt-get update` and `apt-get -s install hello` in a native arm64
# chroot. It then syncs, unmounts, re-checks the filesystem and asserts the image UUID
# is unchanged, so re-running it after a test run reproduces the same state.
#
# The only paths it writes inside an image are /usr/local/share/axvisor-guest-apt/**,
# /etc/apt/axvisor-guest.*, /var/lib/apt/lists-axvisor-guest/** and
# /var/cache/apt/axvisor-guest-*-cache.bin; the dpkg database, the original APT
# sources and lists, the cached .deb and the filesystem UUID are only read or left
# alone. It never creates or restores a backup, never copies a raw image, never
# installs, removes or purges a package, and never rewrites the dpkg database.
#
# Environment overrides:
#   HELLO_PACKAGE  package name to prepare (default: hello)
#   HELLO_VERSION  required cached version (default: 2.10-2ubuntu4, empty accepts
#                  whatever version the image has cached)

set -eu

HELLO_PACKAGE=${HELLO_PACKAGE:-hello}
HELLO_VERSION=${HELLO_VERSION-2.10-2ubuntu4}

REPO_PATH=/usr/local/share/axvisor-guest-apt/repo
SETUP_DIR=/usr/local/share/axvisor-guest-apt
SOURCE_LIST=/etc/apt/axvisor-guest.sources.list
APT_CONFIG_FILE=/etc/apt/axvisor-guest.conf
LISTS_DIR=/var/lib/apt/lists-axvisor-guest
PKG_CACHE=/var/cache/apt/axvisor-guest-pkgcache.bin
SRC_CACHE=/var/cache/apt/axvisor-guest-srcpkgcache.bin

DEFAULT_IMAGES='/guest/rootfs-aarch64-orangepi-jammy-0.img /guest/rootfs-aarch64-orangepi-jammy-1.img'

say() { printf '%s\n' "$*"; }
die() { printf 'prepare-guest-apt-repo: error: %s\n' "$*" >&2; exit 1; }

MNT=
LOOP=
TMP=
UUID_SEEN=
COUNT=0

# Unmount, detach and remove the temporary mount point on any exit path; every step is
# best-effort so the original failure stays the visible one.
cleanup() {
    if [ -n "$MNT" ] && [ -d "$MNT" ] && grep -q " $MNT " /proc/mounts; then
        sync 2>/dev/null || true
        umount "$MNT" 2>/dev/null || umount -l "$MNT" 2>/dev/null || true
    fi
    if [ -n "$LOOP" ]; then
        losetup -d "$LOOP" 2>/dev/null || true
        LOOP=
    fi
    if [ -n "$TMP" ] && [ -d "$TMP" ]; then
        rmdir "$TMP" 2>/dev/null || true
    fi
    MNT=
    TMP=
}
trap cleanup EXIT HUP INT TERM

for tool in blkid chroot dpkg dpkg-deb dpkg-query e2fsck losetup md5sum mktemp \
    mount sha1sum sha256sum stat umount; do
    command -v "$tool" >/dev/null 2>&1 ||
        die "required tool '$tool' is missing on this host"
done

# Build the repository, isolated APT configuration and warm caches inside the image
# already mounted at $MNT.
configure_image() {
    repo_host="$MNT$REPO_PATH"

    say '-- checking the cached .deb and the image package state'
    chroot "$MNT" /bin/sh -c '
        set -eu
        pkg=$1
        status_fmt=$2
        version_fmt=$3
        cache=/var/cache/apt/archives
        set -- "$cache/$pkg"_*.deb
        [ "$#" -eq 1 ] && [ -f "$1" ] || {
            echo "expected exactly one cached $pkg .deb under $cache, found $#: $*" >&2
            exit 1
        }
        if dpkg-query -W -f "$status_fmt" "$pkg" >/dev/null 2>&1; then
            installed=$(dpkg-query -W -f "$version_fmt" "$pkg")
            echo "   note: $pkg $installed is installed; the case will use its reinstall/keep path"
        else
            echo "   note: $pkg is not installed in this image; the case will purge it afterwards"
        fi
    ' sh "$HELLO_PACKAGE" '${Status}' '${Version}' ||
        die "image $IMG has no usable $HELLO_PACKAGE .deb cache; cache it in /var/cache/apt/archives first"

    set -- "$MNT/var/cache/apt/archives/$HELLO_PACKAGE"_*.deb
    [ "$#" -eq 1 ] && [ -f "$1" ] ||
        die "image $IMG must contain exactly one cached $HELLO_PACKAGE .deb under /var/cache/apt/archives (found $#: $*)"
    DEB_SRC=$1
    DEB_NAME=$(basename "$DEB_SRC")

    say '-- verifying the cached deb with the board dpkg-deb'
    DEB_PKG=$(dpkg-deb -f "$DEB_SRC" Package) ||
        die "cannot read the control fields of $DEB_SRC"
    [ "$DEB_PKG" = "$HELLO_PACKAGE" ] ||
        die "$DEB_SRC declares Package: $DEB_PKG, expected $HELLO_PACKAGE"
    DEB_VERSION=$(dpkg-deb -f "$DEB_SRC" Version) ||
        die "cannot read the Version field of $DEB_SRC"
    [ -n "$DEB_VERSION" ] || die "$DEB_SRC has no Version field"
    if [ -n "$HELLO_VERSION" ] && [ "$DEB_VERSION" != "$HELLO_VERSION" ]; then
        die "cached $HELLO_PACKAGE is $DEB_VERSION but HELLO_VERSION=$HELLO_VERSION; refresh the cached .deb or set HELLO_VERSION="
    fi
    DEB_ARCH=$(dpkg-deb -f "$DEB_SRC" Architecture) ||
        die "cannot read the Architecture field of $DEB_SRC"
    HOST_ARCH=$(dpkg --print-architecture)
    [ "$DEB_ARCH" = "$HOST_ARCH" ] ||
        die "$DEB_SRC is built for $DEB_ARCH but this host is $HOST_ARCH; a native chroot needs the same architecture"
    dpkg-deb --contents "$DEB_SRC" >/dev/null ||
        die "$DEB_SRC is not a readable .deb archive"
    say "-- cached deb: $DEB_NAME (package $DEB_PKG, version $DEB_VERSION, arch $DEB_ARCH)"
    say "-- deb depends: $(dpkg-deb -f "$DEB_SRC" Depends)"

    say '-- building the isolated local repository'
    install -d -m 0755 "$MNT$SETUP_DIR" "$repo_host"
    rm -f "$repo_host/${HELLO_PACKAGE}"_*.deb "$repo_host/Packages"
    cp -f "$DEB_SRC" "$repo_host/$DEB_NAME"
    chmod 0644 "$repo_host/$DEB_NAME"
    {
        dpkg-deb -f "$repo_host/$DEB_NAME"
        printf 'Filename: ./%s\n' "$DEB_NAME"
        printf 'Size: %s\n' "$(stat -c %s "$repo_host/$DEB_NAME")"
        printf 'MD5sum: %s\n' "$(md5sum "$repo_host/$DEB_NAME" | cut -d' ' -f1)"
        printf 'SHA1: %s\n' "$(sha1sum "$repo_host/$DEB_NAME" | cut -d' ' -f1)"
        printf 'SHA256: %s\n' "$(sha256sum "$repo_host/$DEB_NAME" | cut -d' ' -f1)"
        printf '\n'
    } > "$repo_host/Packages"

    for field in MD5sum SHA1 SHA256; do
        recorded=$(sed -n "s/^$field: //p" "$repo_host/Packages" | head -n 1)
        case $field in
            MD5sum) actual=$(md5sum "$repo_host/$DEB_NAME" | cut -d' ' -f1) ;;
            SHA1) actual=$(sha1sum "$repo_host/$DEB_NAME" | cut -d' ' -f1) ;;
            SHA256) actual=$(sha256sum "$repo_host/$DEB_NAME" | cut -d' ' -f1) ;;
        esac
        [ -n "$recorded" ] && [ "$recorded" = "$actual" ] ||
            die "generated Packages $field does not match $DEB_NAME"
    done
    say "-- repository index: $REPO_PATH/Packages ($(stat -c %s "$repo_host/Packages") bytes for $(stat -c %s "$repo_host/$DEB_NAME") bytes of .deb)"

    say '-- writing the isolated APT configuration'
    printf 'deb [trusted=yes] file:%s ./\n' "$REPO_PATH" > "$MNT$SOURCE_LIST"
    {
        printf 'Dir::Etc::sourcelist "%s";\n' "$SOURCE_LIST"
        printf 'Dir::Etc::sourceparts "-";\n'
        printf 'Dir::State::lists "%s";\n' "$LISTS_DIR"
        printf 'Dir::Cache::pkgcache "%s";\n' "$PKG_CACHE"
        printf 'Dir::Cache::srcpkgcache "%s";\n' "$SRC_CACHE"
        printf 'APT::Get::List-Cleanup "0";\n'
    } > "$MNT$APT_CONFIG_FILE"
    rm -rf "$MNT$LISTS_DIR"
    install -d -m 0755 "$MNT$LISTS_DIR" "$MNT$LISTS_DIR/partial"
    say "-- APT_CONFIG file: $APT_CONFIG_FILE (lists: $LISTS_DIR)"

    say '-- apt-get update and offline resolution check inside the image'
    chroot "$MNT" /bin/sh -c '
        set -eu
        conf=$1
        repo=$2
        pkg=$3
        APT_CONFIG=$conf
        export APT_CONFIG
        apt-get update
        apt-get -s -y --no-install-recommends install "$pkg"
        apt-cache policy "$pkg" | grep -q "$repo"
        echo "   apt resolves $pkg from file:$repo only"
    ' sh "$APT_CONFIG_FILE" "$REPO_PATH" "$HELLO_PACKAGE" ||
        die "apt preparation inside $IMG failed; see the apt output above"

    say "-- prepared index size: $(du -sh "$MNT$LISTS_DIR" 2>/dev/null | cut -f1)"
    say "-- untouched original indexes: $(du -sh "$MNT/var/lib/apt/lists" 2>/dev/null | cut -f1)"
}

# Refuse an unclean or in-use image, run configure_image on it, then prove the
# filesystem and UUID are unchanged.
prepare_image() {
    IMG=$1
    say "== $IMG"

    [ -f "$IMG" ] ||
        die "missing image $IMG (back it up, run e2fsck, and cache the $HELLO_PACKAGE .deb inside it first)"
    [ -z "$(losetup -j "$IMG" 2>/dev/null)" ] ||
        die "$IMG is already attached to a loop device; stop the guests and detach it first"

    say '-- read-only filesystem check'
    e2fsck -n -f "$IMG" >/dev/null 2>&1 ||
        die "$IMG is not a clean filesystem; back it up and run 'e2fsck -f -y $IMG' first"

    LOOP=$(losetup --find --show "$IMG") ||
        die "cannot attach $IMG to a loop device"
    UUID_BEFORE=$(blkid -o value -s UUID "$LOOP" || true)
    [ -n "$UUID_BEFORE" ] || die "cannot read the filesystem UUID of $IMG"
    say "-- filesystem UUID before: $UUID_BEFORE"

    TMP=$(mktemp -d /tmp/axvisor-guest-apt.XXXXXX) ||
        die "cannot create a temporary mount point"
    MNT=$TMP
    mount -o rw "$LOOP" "$MNT" || die "cannot mount $IMG read-write"

    configure_image

    say '-- sync and unmount'
    sync
    umount "$MNT" || die "cannot unmount $IMG"
    MNT=
    rmdir "$TMP" 2>/dev/null || true
    TMP=

    UUID_AFTER=$(blkid -o value -s UUID "$LOOP" || true)
    [ "$UUID_BEFORE" = "$UUID_AFTER" ] ||
        die "$IMG filesystem UUID changed from $UUID_BEFORE to $UUID_AFTER; restore it from the backup"
    say "-- filesystem UUID after:  $UUID_AFTER (unchanged)"

    e2fsck -n -f "$LOOP" >/dev/null 2>&1 ||
        die "$IMG needs filesystem attention after preparation; inspect 'e2fsck -n -f $IMG'"

    losetup -d "$LOOP" || die "cannot detach the loop device of $IMG"
    LOOP=

    case " $UUID_SEEN " in
        *" $UUID_AFTER "*)
            die "two prepared images share filesystem UUID $UUID_AFTER; keep one independent UUID per image"
            ;;
    esac
    UUID_SEEN="$UUID_SEEN $UUID_AFTER"
    COUNT=$((COUNT + 1))
    say "== $IMG prepared (UUID $UUID_AFTER)"
}

# Prepare every argument image, or the two standard guest images when called without
# arguments.
if [ "$#" -gt 0 ]; then
    IMAGES=$*
else
    IMAGES=$DEFAULT_IMAGES
fi

say "prepare-guest-apt-repo: package=$HELLO_PACKAGE version=${HELLO_VERSION:-any}"
for image in $IMAGES; do
    prepare_image "$image"
done
say "done: $COUNT image(s) prepared; the triple-guest-functional apt steps can now run with APT_CONFIG=$APT_CONFIG_FILE"
