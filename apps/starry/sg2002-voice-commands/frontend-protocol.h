// SPDX-License-Identifier: Apache-2.0
#ifndef VOICE_FRONTEND_PROTOCOL_H
#define VOICE_FRONTEND_PROTOCOL_H

#include <stdint.h>

// Private, local little-endian protocol for the pinned batch-one encoder.
// After the header, each request is mel + cache; each response is features + cache.
// Payloads are contiguous float32 values, with no vendor-owned pointers crossing IPC.
enum {
    VOICE_MEL_COUNT = 45 * 80,
    VOICE_CACHE_COUNT = 128 * 3 * 19,
    VOICE_FEATURE_COUNT = 16 * 128,
};
static const uint32_t voice_frontend_header[] = {
    0x46545056, VOICE_MEL_COUNT, VOICE_CACHE_COUNT, VOICE_FEATURE_COUNT, VOICE_CACHE_COUNT,
};

#endif
