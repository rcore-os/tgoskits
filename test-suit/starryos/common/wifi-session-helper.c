#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/socket.h>
#include <unistd.h>

#include "../../../os/StarryOS/uapi/wireless_compat.h"

static void wipe(void *memory, size_t length) {
    volatile unsigned char *bytes = memory;
    while (length-- != 0) {
        *bytes++ = 0;
    }
}

static int read_secret(const char *path, void *buffer, size_t capacity, size_t *length) {
    FILE *file = fopen(path, "rb");
    if (file == NULL) return -1;
    *length = fread(buffer, 1, capacity, file);
    int failed = ferror(file) || !feof(file);
    fclose(file);
    unlink(path);
    if (failed || *length == 0 || *length == capacity) return -1;
    return 0;
}

static void init_request(struct iwreq_compat *request) {
    memset(request, 0, sizeof(*request));
    memcpy(request->ifrn_name, "wlan0", 6);
}

static int set_mode(int socket_fd) {
    struct iwreq_compat request;
    init_request(&request);
    request.u.mode = IW_MODE_INFRA;
    return ioctl(socket_fd, SIOCSIWMODE, &request);
}

static int set_point(int socket_fd, unsigned long command, const void *value, size_t length) {
    struct iwreq_compat request;
    init_request(&request);
    request.u.point.pointer = (void *)value;
    request.u.point.length = (uint16_t)length;
    request.u.point.flags = command == SIOCSIWESSID;
    return ioctl(socket_fd, command, &request);
}

static int set_pmk(int socket_fd, const uint8_t pmk[WPA2_PMK_SIZE]) {
    struct iw_encode_ext_compat encoded;
    memset(&encoded, 0, sizeof(encoded));
    encoded.alg = IW_ENCODE_ALG_PMK;
    encoded.key_len = WPA2_PMK_SIZE;
    memcpy(encoded.key, pmk, WPA2_PMK_SIZE);
    int result = set_point(socket_fd, SIOCSIWENCODEEXT, &encoded, sizeof(encoded));
    wipe(&encoded, sizeof(encoded));
    return result;
}

static int commit(int socket_fd) {
    struct iwreq_compat request;
    init_request(&request);
    return ioctl(socket_fd, SIOCSIWCOMMIT, &request);
}

int main(int argc, char **argv) {
    char ssid[33];
    uint8_t pmk[WPA2_PMK_SIZE + 1];
    size_t ssid_length = 0;
    size_t pmk_length = 0;
    int result = 1;
    int socket_fd = -1;
    const char *failed_operation = "socket";

    if (argc != 3
        || read_secret(argv[1], ssid, sizeof(ssid), &ssid_length) != 0
        || read_secret(argv[2], pmk, sizeof(pmk), &pmk_length) != 0
        || ssid_length > 32
        || pmk_length != WPA2_PMK_SIZE) {
        fprintf(stderr, "wifi-session-helper: invalid credential files\n");
        goto out;
    }
    socket_fd = socket(AF_INET, SOCK_DGRAM, 0);
    if (socket_fd < 0) {
        goto wext_failed;
    }
    failed_operation = "mode";
    if (set_mode(socket_fd) != 0) {
        goto wext_failed;
    }
    failed_operation = "ESSID";
    if (set_point(socket_fd, SIOCSIWESSID, ssid, ssid_length) != 0) {
        goto wext_failed;
    }
    failed_operation = "PMK";
    if (set_pmk(socket_fd, pmk) != 0) {
        goto wext_failed;
    }
    failed_operation = "commit";
    if (commit(socket_fd) != 0) {
        goto wext_failed;
    }
    puts("STARRY_WIFI_CONTROL_COMPLETE");
    result = 0;
    goto out;

wext_failed:
    {
        int failure_errno = errno;
        fprintf(stderr, "wifi-session-helper: WEXT %s failed: %s\n",
                failed_operation, strerror(failure_errno));
        goto out;
    }
out:
    if (socket_fd >= 0) close(socket_fd);
    if (argc == 3) {
        unlink(argv[1]);
        unlink(argv[2]);
    }
    wipe(pmk, sizeof(pmk));
    wipe(ssid, sizeof(ssid));
    return result;
}
