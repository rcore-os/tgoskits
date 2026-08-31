#ifndef STARRY_WIRELESS_COMPAT_H
#define STARRY_WIRELESS_COMPAT_H

#include <stddef.h>
#include <stdint.h>
#include <sys/socket.h>

/* Linux wireless-extensions UAPI subset implemented by StarryOS. */
#define SIOCSIWCOMMIT 0x8B00
#define SIOCSIWFREQ 0x8B04
#define SIOCSIWMODE 0x8B06
#define SIOCSIWESSID 0x8B1A
#define SIOCSIWENCODEEXT 0x8B34

#define IW_MODE_INFRA 2
#define IW_MODE_MASTER 3
#define IW_ENCODE_ALG_PMK 4
#define IW_ENCODE_SEQ_MAX_SIZE 8
#define IW_ESSID_MAX_SIZE 32
#define WPA2_PMK_SIZE 32

#ifndef IFNAMSIZ
#define IFNAMSIZ 16
#endif

struct iw_point_compat {
    void *pointer;
    uint16_t length;
    uint16_t flags;
};

struct iw_freq_compat {
    int32_t mantissa;
    int16_t exponent;
    uint8_t index;
    uint8_t flags;
};

struct iwreq_compat {
    char ifrn_name[IFNAMSIZ];
    union {
        uint32_t mode;
        struct iw_freq_compat freq;
        struct iw_point_compat point;
        char pad[16];
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
_Static_assert(offsetof(struct iwreq_compat, u.point.length) == 24,
               "Linux iw_point length offset");
_Static_assert(offsetof(struct iwreq_compat, u.point.flags) == 26,
               "Linux iw_point flags offset");

#endif
