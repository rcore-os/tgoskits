/* SPDX-License-Identifier: Apache-2.0 */

#ifndef STARRY_AXIVC_H
#define STARRY_AXIVC_H

#include "tgos_ivc_app.h"

#include <stddef.h>
#include <stdint.h>
#include <sys/ioctl.h>

/*
 * The current StarryOS direct auto-provision endpoint is /dev/axivc0. The
 * legacy manager at /dev/axivc still requires publish/subscribe ioctls and is
 * not the one-channel application ABI used by these samples.
 */
#ifndef TGOS_STARRY_AXIVC_DEVICE
#define TGOS_STARRY_AXIVC_DEVICE "/dev/axivc0"
#endif

#define TGOS_STARRY_AXIVC_PAYLOAD_INFO_VERSION 1ULL

struct starry_axivc_payload_info {
  uint64_t version;
  uint64_t generation;
  uint64_t local_offset;
  uint64_t peer_offset;
  uint64_t payload_size;
};

#define TGOS_STARRY_AXIVC_PAYLOAD_INFO                                         \
  _IOR(0x49, 0x06, struct starry_axivc_payload_info)

_Static_assert(sizeof(struct starry_axivc_payload_info) == 0x28,
               "payload info ABI must remain 40 bytes");
_Static_assert((uint32_t)TGOS_STARRY_AXIVC_PAYLOAD_INFO == 0x80284906U,
               "payload info ioctl must remain _IOR(0x49, 6, payload_info)");

struct starry_axivc {
  int fd;
  size_t capacity;
  struct starry_axivc_payload_info info;
  uint8_t *local_payload;
  const uint8_t *peer_payload;
};

int starry_axivc_open(struct starry_axivc *ivc, size_t minimum_capacity);
void starry_axivc_close(struct starry_axivc *ivc);
int starry_axivc_send_frame(struct starry_axivc *ivc,
                            const struct tgos_ivc_frame *frame,
                            int32_t timeout_ms);
int starry_axivc_recv_frame(struct starry_axivc *ivc,
                            struct tgos_ivc_frame *frame, int32_t timeout_ms);
uint64_t starry_now_us(void);
const char *starry_error_name(int error);

#endif /* STARRY_AXIVC_H */
