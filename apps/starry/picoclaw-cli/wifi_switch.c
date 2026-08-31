/*
 * wifi_switch — runtime Wi-Fi mode switch demo for StarryOS (sg2002 / aic8800).
 *
 * Drives the kernel's wireless-extensions ioctl path (see
 * os/StarryOS/kernel/src/file/wext.rs) to switch the wlan0 interface between
 * Station and SoftAP at runtime. Setters stage config; SIOCSIWCOMMIT applies it
 * atomically (link-layer VIF teardown + switch + IP/DHCP role reconfig).
 *
 * Build (riscv64, musl static — matches the other sg2002 rootfs binaries):
 *   riscv64-linux-musl-gcc -static -O2 -o wifi_switch wifi_switch.c
 * Then drop it into the p3 rootfs at /usr/bin/wifi_switch (chmod +x) alongside
 * tennis/test_motor/etc. See docs/sd-card-build.md.
 *
 * Usage on the board:
 *   wifi_switch ap   <ssid> [channel]      # become open SoftAP (default ch 6)
 *   wifi_switch sta  <ssid> [pmk-file]     # join WPA2, or omit for open
 *
 * We deliberately avoid <linux/wireless.h> (the cross toolchain may lack it)
 * and lay out `struct iwreq` by hand. The layout below MUST match wext.rs:
 *   - ifr name      : offset 0,  16 bytes
 *   - iwreq_data    : offset 16, 16-byte union
 *   - MODE          : first u32 of the union
 *   - FREQ          : Linux iw_freq { s32 m; s16 e; u8 i; u8 flags; }
 *   - ESSID/ENCODE  : iw_point { void *pointer; __u16 length; __u16 flags; }
 *                     pointer @ union+0 (8B on rv64), length @ union+8
 *   - ENCODE payload: Linux struct iw_encode_ext followed by a 32-byte PMK
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <stddef.h>
#include <fcntl.h>
#include <unistd.h>
#include <errno.h>
#include <sys/ioctl.h>
#include <sys/socket.h>

#include "../../../os/StarryOS/uapi/wireless_compat.h"

static void wipe(void *memory, size_t length) {
    volatile unsigned char *bytes = memory;
    while (length-- != 0) *bytes++ = 0;
}

static int wext(int fd, unsigned long cmd, struct iwreq_compat *req) {
    if (ioctl(fd, cmd, req) < 0) {
        fprintf(stderr, "ioctl 0x%lx failed: %s\n", cmd, strerror(errno));
        return -1;
    }
    return 0;
}

static void set_ifname(struct iwreq_compat *req, const char *ifname) {
    memset(req, 0, sizeof(*req));
    strncpy(req->ifrn_name, ifname, IFNAMSIZ - 1);
}

static int do_set_mode(int fd, const char *ifname, uint32_t mode) {
    struct iwreq_compat req;
    set_ifname(&req, ifname);
    req.u.mode = mode;
    return wext(fd, SIOCSIWMODE, &req);
}

static int do_set_essid(int fd, const char *ifname, const char *ssid) {
    struct iwreq_compat req;
    size_t len = strlen(ssid);
    if (len > IW_ESSID_MAX_SIZE) {
        fprintf(stderr, "ssid too long (max %d)\n", IW_ESSID_MAX_SIZE);
        return -1;
    }
    set_ifname(&req, ifname);
    req.u.point.pointer = (void *)ssid;
    req.u.point.length = (uint16_t)len;
    req.u.point.flags = 1; /* SSID active */
    return wext(fd, SIOCSIWESSID, &req);
}

static int do_set_pmk(int fd, const char *ifname, const uint8_t pmk[WPA2_PMK_SIZE]) {
    struct iwreq_compat req;
    struct iw_encode_ext_compat encoded;
    memset(&encoded, 0, sizeof(encoded));
    encoded.alg = IW_ENCODE_ALG_PMK;
    encoded.key_len = WPA2_PMK_SIZE;
    memcpy(encoded.key, pmk, WPA2_PMK_SIZE);
    set_ifname(&req, ifname);
    req.u.point.pointer = &encoded;
    req.u.point.length = sizeof(encoded);
    req.u.point.flags = 0;
    int result = wext(fd, SIOCSIWENCODEEXT, &req);
    wipe(&encoded, sizeof(encoded));
    return result;
}

static int read_pmk_file(const char *path, uint8_t pmk[WPA2_PMK_SIZE]) {
    int flags = O_RDONLY;
#ifdef O_CLOEXEC
    flags |= O_CLOEXEC;
#endif
#ifdef O_NOFOLLOW
    flags |= O_NOFOLLOW;
#endif
    int pmk_fd = open(path, flags);
    if (pmk_fd < 0) {
        fprintf(stderr, "failed to open PMK file: %s\n", strerror(errno));
        return -1;
    }

    size_t offset = 0;
    while (offset < WPA2_PMK_SIZE) {
        ssize_t count = read(pmk_fd, pmk + offset, WPA2_PMK_SIZE - offset);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) goto invalid;
        offset += (size_t)count;
    }

    uint8_t extra;
    ssize_t count;
    do {
        count = read(pmk_fd, &extra, sizeof(extra));
    } while (count < 0 && errno == EINTR);
    if (count != 0) goto invalid;
    close(pmk_fd);
    return 0;

invalid:
    fprintf(stderr, "PMK file must contain exactly %d raw bytes\n", WPA2_PMK_SIZE);
    wipe(pmk, WPA2_PMK_SIZE);
    close(pmk_fd);
    return -1;
}

static int do_set_channel(int fd, const char *ifname, uint32_t chan) {
    struct iwreq_compat req;
    set_ifname(&req, ifname);
    req.u.freq.mantissa = (int32_t)chan;
    req.u.freq.exponent = 0;
    return wext(fd, SIOCSIWFREQ, &req);
}

static int do_commit(int fd, const char *ifname) {
    struct iwreq_compat req;
    set_ifname(&req, ifname);
    return wext(fd, SIOCSIWCOMMIT, &req);
}

static void usage(const char *argv0) {
    fprintf(stderr,
        "usage:\n"
        "  %s ap  <ssid> [channel]      become open SoftAP (default channel 6)\n"
        "  %s sta <ssid> [pmk-file]     join WPA2, or omit for open\n",
        argv0, argv0);
}

int main(int argc, char **argv) {
    const char *ifname = "wlan0";

    if (argc < 3) {
        usage(argv[0]);
        return 2;
    }

    int fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (fd < 0) {
        perror("socket");
        return 1;
    }

    const char *mode = argv[1];
    const char *ssid = argv[2];
    uint8_t pmk[WPA2_PMK_SIZE] = {0};
    int rc = 1;

    if (strcmp(mode, "ap") == 0) {
        uint32_t chan = (argc >= 4) ? (uint32_t)atoi(argv[3]) : 6;
        printf("[wifi_switch] %s -> SoftAP channel=%u\n", ifname, chan);
        if (do_set_mode(fd, ifname, IW_MODE_MASTER)) goto out;
        if (do_set_essid(fd, ifname, ssid)) goto out;
        if (do_set_channel(fd, ifname, chan)) goto out;
        if (do_commit(fd, ifname)) goto out;
        printf("[wifi_switch] SoftAP commit OK\n");
        rc = 0;
    } else if (strcmp(mode, "sta") == 0) {
        const char *pmk_file = (argc >= 4) ? argv[3] : "";
        printf("[wifi_switch] %s -> Station (%s)\n", ifname, pmk_file[0] ? "wpa2" : "open");
        if (pmk_file[0] && read_pmk_file(pmk_file, pmk)) goto out;
        if (do_set_mode(fd, ifname, IW_MODE_INFRA)) goto out;
        if (do_set_essid(fd, ifname, ssid)) goto out;
        if (pmk_file[0] && do_set_pmk(fd, ifname, pmk)) goto out;
        if (do_commit(fd, ifname)) goto out;
        printf("[wifi_switch] Station commit OK\n");
        rc = 0;
    } else {
        usage(argv[0]);
        rc = 2;
    }

out:
    wipe(pmk, sizeof(pmk));
    close(fd);
    return rc;
}
