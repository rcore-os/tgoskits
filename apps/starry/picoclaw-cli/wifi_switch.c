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
 *   wifi_switch sta  <ssid> [passphrase]   # join a network in station mode
 *
 * We deliberately avoid <linux/wireless.h> (the cross toolchain may lack it)
 * and lay out `struct iwreq` by hand. The layout below MUST match wext.rs:
 *   - ifr name      : offset 0,  16 bytes
 *   - iwreq_data    : offset 16, 16-byte union
 *   - MODE / FREQ   : first u32 of the union
 *   - ESSID/ENCODE  : iw_point { void *pointer; __u16 length; __u16 flags; }
 *                     pointer @ union+0 (8B on rv64), length @ union+8
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <unistd.h>
#include <errno.h>
#include <sys/ioctl.h>
#include <sys/socket.h>

/* Wireless-extensions ioctl numbers (from <linux/wireless.h>). */
#define SIOCSIWCOMMIT     0x8B00
#define SIOCSIWFREQ       0x8B04
#define SIOCSIWMODE       0x8B06
#define SIOCSIWESSID      0x8B1A
#define SIOCSIWENCODEEXT  0x8B34

/* iw_mode values. */
#define IW_MODE_INFRA     2  /* Managed / Station */
#define IW_MODE_MASTER    3  /* Master  / Access Point */

#define IFNAMSIZ          16
#define IW_ESSID_MAX_SIZE 32

/* Hand-rolled iw_point: { void *pointer; __u16 length; __u16 flags; }. */
struct iw_point_compat {
    void    *pointer;
    uint16_t length;
    uint16_t flags;
};

/*
 * Hand-rolled iwreq: 16-byte name union, then a 16-byte iwreq_data union.
 * We only ever use the u32 field (mode/freq) or the iw_point field (essid/key).
 */
struct iwreq_compat {
    char ifrn_name[IFNAMSIZ];
    union {
        uint32_t                mode;     /* SIOCSIWMODE / SIOCSIWFREQ */
        struct iw_point_compat  essid;    /* SIOCSIWESSID / ...ENCODEEXT */
        char                    pad[16];  /* keep the union exactly 16 bytes */
    } u;
};

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
    req.u.essid.pointer = (void *)ssid;
    req.u.essid.length = (uint16_t)len;
    req.u.essid.flags = 1; /* SSID active */
    return wext(fd, SIOCSIWESSID, &req);
}

static int do_set_key(int fd, const char *ifname, const char *pass) {
    struct iwreq_compat req;
    set_ifname(&req, ifname);
    req.u.essid.pointer = (void *)pass;
    req.u.essid.length = (uint16_t)strlen(pass);
    req.u.essid.flags = 0;
    return wext(fd, SIOCSIWENCODEEXT, &req);
}

static int do_set_channel(int fd, const char *ifname, uint32_t chan) {
    struct iwreq_compat req;
    set_ifname(&req, ifname);
    req.u.mode = chan; /* wext.rs reads first u32 of the union as channel */
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
        "  %s sta <ssid> [passphrase]   join a network in station mode\n",
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
    int rc = 1;

    if (strcmp(mode, "ap") == 0) {
        uint32_t chan = (argc >= 4) ? (uint32_t)atoi(argv[3]) : 6;
        printf("[wifi_switch] %s -> SoftAP ssid=\"%s\" channel=%u\n", ifname, ssid, chan);
        if (do_set_mode(fd, ifname, IW_MODE_MASTER)) goto out;
        if (do_set_essid(fd, ifname, ssid)) goto out;
        if (do_set_channel(fd, ifname, chan)) goto out;
        if (do_commit(fd, ifname)) goto out;
        printf("[wifi_switch] SoftAP commit OK\n");
        rc = 0;
    } else if (strcmp(mode, "sta") == 0) {
        const char *pass = (argc >= 4) ? argv[3] : "";
        printf("[wifi_switch] %s -> Station ssid=\"%s\" (%s)\n",
               ifname, ssid, pass[0] ? "wpa2" : "open");
        if (do_set_mode(fd, ifname, IW_MODE_INFRA)) goto out;
        if (do_set_essid(fd, ifname, ssid)) goto out;
        if (pass[0] && do_set_key(fd, ifname, pass)) goto out;
        if (do_commit(fd, ifname)) goto out;
        printf("[wifi_switch] Station commit OK\n");
        rc = 0;
    } else {
        usage(argv[0]);
        rc = 2;
    }

out:
    close(fd);
    return rc;
}
