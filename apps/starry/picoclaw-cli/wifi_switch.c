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
 *   wifi_switch sta  <ssid> [pmk-hex]      # join WPA2, or omit for open
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
#define IW_ENCODE_ALG_PMK 4
#define IW_ENCODE_SEQ_MAX_SIZE 8

#define IFNAMSIZ          16
#define IW_ESSID_MAX_SIZE 32
#define WPA2_PMK_SIZE     32

/* Hand-rolled iw_point: { void *pointer; __u16 length; __u16 flags; }. */
struct iw_point_compat {
    void    *pointer;
    uint16_t length;
    uint16_t flags;
};

struct iw_freq_compat {
    int32_t mantissa;
    int16_t exponent;
    uint8_t index;
    uint8_t flags;
};

/*
 * Hand-rolled iwreq: 16-byte name union, then a 16-byte iwreq_data union.
 * We only ever use the u32 field (mode/freq) or the iw_point field (essid/key).
 */
struct iwreq_compat {
    char ifrn_name[IFNAMSIZ];
    union {
        uint32_t                mode;     /* SIOCSIWMODE */
        struct iw_freq_compat   freq;     /* SIOCSIWFREQ */
        struct iw_point_compat  essid;    /* SIOCSIWESSID / ...ENCODEEXT */
        char                    pad[16];  /* keep the union exactly 16 bytes */
    } u;
};

struct iw_encode_ext_compat {
    uint32_t ext_flags;
    uint8_t tx_seq[IW_ENCODE_SEQ_MAX_SIZE];
    uint8_t rx_seq[IW_ENCODE_SEQ_MAX_SIZE];
    struct sockaddr addr;
    uint16_t alg;
    uint16_t key_len;
    uint8_t key[WPA2_PMK_SIZE];
};

_Static_assert(offsetof(struct iw_encode_ext_compat, alg) == 36,
               "Linux iw_encode_ext alg offset");
_Static_assert(offsetof(struct iw_encode_ext_compat, key_len) == 38,
               "Linux iw_encode_ext key_len offset");
_Static_assert(offsetof(struct iw_encode_ext_compat, key) == 40,
               "Linux iw_encode_ext key offset");
_Static_assert(sizeof(struct iwreq_compat) == 32, "Linux iwreq size on LP64");
_Static_assert(offsetof(struct iwreq_compat, u) == 16, "Linux iwreq data offset");
_Static_assert(offsetof(struct iwreq_compat, u.essid.length) == 24,
               "Linux iw_point length offset");
_Static_assert(offsetof(struct iwreq_compat, u.essid.flags) == 26,
               "Linux iw_point flags offset");

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
    req.u.essid.pointer = (void *)ssid;
    req.u.essid.length = (uint16_t)len;
    req.u.essid.flags = 1; /* SSID active */
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
    req.u.essid.pointer = &encoded;
    req.u.essid.length = sizeof(encoded);
    req.u.essid.flags = 0;
    int result = wext(fd, SIOCSIWENCODEEXT, &req);
    wipe(&encoded, sizeof(encoded));
    return result;
}

static int hex_nibble(char value) {
    if (value >= '0' && value <= '9') return value - '0';
    if (value >= 'a' && value <= 'f') return value - 'a' + 10;
    if (value >= 'A' && value <= 'F') return value - 'A' + 10;
    return -1;
}

static int decode_pmk(const char *hex, uint8_t pmk[WPA2_PMK_SIZE]) {
    if (strlen(hex) != WPA2_PMK_SIZE * 2) return -1;
    for (size_t index = 0; index < WPA2_PMK_SIZE; index++) {
        int high = hex_nibble(hex[index * 2]);
        int low = hex_nibble(hex[index * 2 + 1]);
        if (high < 0 || low < 0) return -1;
        pmk[index] = (uint8_t)((high << 4) | low);
    }
    return 0;
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
        "  %s sta <ssid> [pmk-hex]      join WPA2, or omit for open\n",
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
        const char *pmk_hex = (argc >= 4) ? argv[3] : "";
        printf("[wifi_switch] %s -> Station (%s)\n", ifname, pmk_hex[0] ? "wpa2" : "open");
        if (pmk_hex[0] && decode_pmk(pmk_hex, pmk)) {
            fprintf(stderr, "pmk must contain exactly 64 hexadecimal digits\n");
            goto out;
        }
        if (do_set_mode(fd, ifname, IW_MODE_INFRA)) goto out;
        if (do_set_essid(fd, ifname, ssid)) goto out;
        if (pmk_hex[0] && do_set_pmk(fd, ifname, pmk)) goto out;
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
