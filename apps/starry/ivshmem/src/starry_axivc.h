/* SPDX-License-Identifier: Apache-2.0 */

#ifndef STARRY_AXIVC_H
#define STARRY_AXIVC_H

#include "ivc.h"
#include "tgos_ivc_app.h"

#include <stddef.h>
#include <stdint.h>

/*
 * The current StarryOS direct auto-provision endpoint is /dev/axivc0. The
 * legacy manager at /dev/axivc still requires publish/subscribe ioctls and is
 * not the one-channel application ABI used by these samples.
 */
#ifndef TGOS_STARRY_AXIVC_DEVICE
#define TGOS_STARRY_AXIVC_DEVICE "/dev/axivc0"
#endif

_Static_assert(IVC_MAX_RECORD_BYTES == TGOS_IVC_FRAME_BYTES,
               "common IVC and application frame record limits must agree");
_Static_assert(IVC_MAX_TIMEOUT_MS == TGOS_IVC_TIMEOUT_MS,
               "common IVC and application timeout limits must agree");

/*
 * The view borrows the connection-owned mappings. The local window is
 * writable and the peer window is read-only; both become invalid at ivc_close.
 */
struct starry_axivc_payload_view {
  uint64_t generation;
  uint64_t local_offset;
  uint64_t peer_offset;
  size_t capacity;
  uint8_t *local_payload;
  const uint8_t *peer_payload;
};

int starry_axivc_payload_get(struct ivc *connection,
                             size_t minimum_capacity,
                             struct starry_axivc_payload_view *view);
uint64_t starry_now_us(void);
const char *starry_error_name(int error);

#endif /* STARRY_AXIVC_H */
