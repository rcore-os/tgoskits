#!/usr/bin/env bash
set -euo pipefail

# Build a tiny x86_64 Linux initramfs for Axvisor phase-0 bring-up.
#
# The archive contains a tiny static /init that mounts devtmpfs, opens
# /dev/console, writes one line, optionally reads one sector from /dev/vda, and
# then idles forever.  The block check is read-only and non-fatal so the same
# initramfs can serve early bring-up and PCI/virtio-blk smoke tests.  Serial
# output is provided by the kernel command line's console=ttyS0 setting rather
# than by userspace I/O port access.

OUT="${1:-tmp/linux-x86_64/initramfs.cpio}"
WORKDIR="$(mktemp -d)"
trap 'rm -rf "${WORKDIR}"' EXIT

case "${OUT}" in
  /*) OUT_ABS="${OUT}" ;;
  *) OUT_ABS="${PWD}/${OUT}" ;;
esac

mkdir -p "${WORKDIR}/root/dev" "${WORKDIR}/root/proc" "${WORKDIR}/root/sys" "$(dirname "${OUT_ABS}")"

cat > "${WORKDIR}/init.c" <<'INIT_EOF'
#define SYS_read 0
#define SYS_write 1
#define SYS_open 2
#define SYS_close 3
#define SYS_mount 165
#define SYS_dup2 33
#define SYS_nanosleep 35

#define O_RDONLY 0
#define O_RDWR 02

struct timespec {
    long tv_sec;
    long tv_nsec;
};

static long syscall6(long nr, long a0, long a1, long a2, long a3, long a4, long a5) {
    long ret;
    register long r10 __asm__("r10") = a3;
    register long r8 __asm__("r8") = a4;
    register long r9 __asm__("r9") = a5;
    __asm__ volatile (
        "syscall"
        : "=a"(ret)
        : "a"(nr), "D"(a0), "S"(a1), "d"(a2), "r"(r10), "r"(r8), "r"(r9)
        : "rcx", "r11", "memory"
    );
    return ret;
}

static long sys_write(long fd, const char *buf, long len) {
    return syscall6(SYS_write, fd, (long)buf, len, 0, 0, 0);
}

static long sys_open(const char *path, long flags, long mode) {
    return syscall6(SYS_open, (long)path, flags, mode, 0, 0, 0);
}

static long sys_mount(const char *src, const char *target, const char *fstype) {
    return syscall6(SYS_mount, (long)src, (long)target, (long)fstype, 0, 0, 0);
}

static long sys_read(long fd, char *buf, long len) {
    return syscall6(SYS_read, fd, (long)buf, len, 0, 0, 0);
}

static void sys_dup2(long oldfd, long newfd) {
    syscall6(SYS_dup2, oldfd, newfd, 0, 0, 0, 0);
}

static void sleep_forever(void) {
    struct timespec ts = { .tv_sec = 3600, .tv_nsec = 0 };
    for (;;) {
        syscall6(SYS_nanosleep, (long)&ts, 0, 0, 0, 0, 0);
    }
}

void _start(void) {
    const char msg[] = "axvisor x86_64 linux initramfs reached /init\n";
    const char blk_ok[] = "axvisor x86_64 linux virtio-blk read /dev/vda ok\n";
    const char blk_skip[] = "axvisor x86_64 linux virtio-blk /dev/vda not ready\n";
    char sector[512];

    sys_mount("devtmpfs", "/dev", "devtmpfs");
    long kmsg = sys_open("/dev/kmsg", O_RDWR, 0);
    if (kmsg >= 0) {
        sys_write(kmsg, msg, sizeof(msg) - 1);
    }
    long console = sys_open("/dev/console", O_RDWR, 0);
    if (console < 0) {
        console = sys_open("/dev/ttyS0", O_RDWR, 0);
    }
    if (console >= 0) {
        sys_dup2(console, 0);
        sys_dup2(console, 1);
        sys_dup2(console, 2);
        sys_write(console, msg, sizeof(msg) - 1);
    }
    sys_write(1, msg, sizeof(msg) - 1);
    sys_write(2, msg, sizeof(msg) - 1);

    long vda = sys_open("/dev/vda", O_RDONLY, 0);
    if (vda >= 0 && sys_read(vda, sector, sizeof(sector)) == sizeof(sector)) {
        if (kmsg >= 0) {
            sys_write(kmsg, blk_ok, sizeof(blk_ok) - 1);
        }
        sys_write(1, blk_ok, sizeof(blk_ok) - 1);
    } else {
        if (kmsg >= 0) {
            sys_write(kmsg, blk_skip, sizeof(blk_skip) - 1);
        }
        sys_write(1, blk_skip, sizeof(blk_skip) - 1);
    }

    sleep_forever();
}
INIT_EOF

command -v cpio >/dev/null 2>&1 || {
  echo "ERROR: cpio is required to build the initramfs" >&2
  exit 1
}

if command -v musl-gcc >/dev/null 2>&1; then
  CC=musl-gcc
else
  CC=${CC:-gcc}
fi

command -v "${CC}" >/dev/null 2>&1 || {
  echo "ERROR: ${CC} is required to build the initramfs" >&2
  exit 1
}

"${CC}" -static -Os -nostdlib -ffreestanding -fno-stack-protector \
  -o "${WORKDIR}/root/init" "${WORKDIR}/init.c"
chmod 0755 "${WORKDIR}/root/init"

(
  cd "${WORKDIR}/root"
  find . -print0 | cpio --null -o --format=newc > "${OUT_ABS}"
)

echo "Wrote ${OUT_ABS}"
