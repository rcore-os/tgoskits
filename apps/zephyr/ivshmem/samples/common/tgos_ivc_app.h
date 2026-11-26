/* SPDX-License-Identifier: Apache-2.0 */

#ifndef TGOS_IVC_APP_H
#define TGOS_IVC_APP_H

#include <stddef.h>
#include <stdint.h>
#include <string.h>

#define TGOS_IVC_FRAME_MAGIC 0x49564754U
#define TGOS_IVC_FRAME_VERSION 1U
#define TGOS_IVC_FRAME_BYTES 48U

#define TGOS_IVC_FRAME_DATA 1U
#define TGOS_IVC_FRAME_ACK 2U
#define TGOS_IVC_FRAME_STATS 3U
#define TGOS_IVC_FRAME_DONE 4U
#define TGOS_IVC_FRAME_FAIL 5U

#define TGOS_IVC_CHUNK_SIZE 4096U
#define TGOS_IVC_BENCH_CASE_COUNT 4U

/*
 * Select the benchmark payload path at build time. Both images must use the
 * same value; the default keeps the established generate-while-sending path.
 */
#ifndef AXIVC_BENCH_PREPARED
#define AXIVC_BENCH_PREPARED 0
#endif

#if AXIVC_BENCH_PREPARED != 0 && AXIVC_BENCH_PREPARED != 1
#error "AXIVC_BENCH_PREPARED must be 0 or 1"
#endif

#define TGOS_IVC_FRAME_FLAG_PREPARED 0x01U
#define TGOS_IVC_PREPARED_MARKER_MAGIC 0x4c454731U
#define TGOS_IVC_PREPARED_MARKER_BYTES 16U
#define TGOS_IVC_PREPARED_MAX_BYTES (10U * 1024U * 1024U)

#ifndef AXIVC_BENCH_ITERATIONS
#define AXIVC_BENCH_ITERATIONS 100U
#endif

#define TGOS_IVC_DEMO_CASE_COUNT 4U
#define TGOS_IVC_TIMEOUT_MS 60000

#if !defined(__BYTE_ORDER__) || !defined(__ORDER_LITTLE_ENDIAN__) ||           \
    !defined(__ORDER_BIG_ENDIAN__)
#error "tgos_ivc_app.h requires compiler byte-order macros"
#elif __BYTE_ORDER__ == __ORDER_LITTLE_ENDIAN__
#define TGOS_IVC_HOST_BIG_ENDIAN 0
#elif __BYTE_ORDER__ == __ORDER_BIG_ENDIAN__
#define TGOS_IVC_HOST_BIG_ENDIAN 1
#else
#error "unsupported host byte order"
#endif

/*
 * Application frames are deliberately small because each frame is carried in
 * one Message V1 fragment. Keep every field naturally aligned so the same
 * layout is valid in StarryOS userspace and Zephyr without packing attributes.
 */
struct tgos_ivc_frame {
  uint32_t magic;
  uint16_t version;
  uint8_t type;
  uint8_t flags;
  uint32_t case_index;
  uint32_t iteration;
  uint32_t payload_len;
  uint32_t chunk_size;
  uint64_t checksum;
  uint64_t total_bytes;
  uint64_t value;
};

_Static_assert(sizeof(struct tgos_ivc_frame) == TGOS_IVC_FRAME_BYTES,
               "application frame must fit one Message V1 fragment");
_Static_assert(AXIVC_BENCH_ITERATIONS > 0U,
               "benchmark iteration count must be positive");

static inline size_t tgos_ivc_bench_size_at(uint32_t case_index) {
  switch (case_index) {
  case 0U:
    return 256U * 1024U;
  case 1U:
    return 512U * 1024U;
  case 2U:
    return 1024U * 1024U;
  default:
    return 10U * 1024U * 1024U;
  }
}

static inline size_t tgos_ivc_demo_size_at(uint32_t case_index) {
  return (size_t)(case_index + 1U) * TGOS_IVC_CHUNK_SIZE;
}

static inline uint64_t tgos_ivc_bench_total_bytes(void) {
  uint64_t total = 0U;

  for (uint32_t index = 0U; index < TGOS_IVC_BENCH_CASE_COUNT; index++) {
    total += (uint64_t)tgos_ivc_bench_size_at(index) *
             (uint64_t)AXIVC_BENCH_ITERATIONS;
  }
  return total;
}

static inline uint64_t tgos_ivc_bench_total_chunks(void) {
  return (uint64_t)TGOS_IVC_BENCH_CASE_COUNT * (uint64_t)AXIVC_BENCH_ITERATIONS;
}

/*
 * These helpers preserve the established little-endian byte sequence while
 * allowing callers to use naturally typed memcpy operations.
 */
static inline uint64_t tgos_ivc_to_le64(uint64_t value) {
#if TGOS_IVC_HOST_BIG_ENDIAN
  return __builtin_bswap64(value);
#else
  return value;
#endif
}

static inline uint32_t tgos_ivc_to_le32(uint32_t value) {
#if TGOS_IVC_HOST_BIG_ENDIAN
  return __builtin_bswap32(value);
#else
  return value;
#endif
}

static inline uint32_t tgos_ivc_fill_next_word(uint32_t *state) {
  uint32_t value = *state;

  value ^= value << 13;
  value ^= value >> 17;
  value ^= value << 5;
  *state = value;
  return value ^ 0xa5c35a3cU;
}

/*
 * Generate one deterministic 4 KiB source chunk. The case, iteration and
 * absolute payload offset all participate in the sequence so stale payloads
 * cannot satisfy a later request.
 */
static inline void tgos_ivc_fill_chunk(uint8_t *dst, size_t len,
                                       uint32_t case_index, uint32_t iteration,
                                       uint32_t offset) {
  uint32_t state = 0x9e3779b9U ^ (case_index * 0x85ebca6bU) ^
                   (iteration * 0xc2b2ae35U) ^ (offset * 0x27d4eb2fU);
  size_t index = 0U;

  while (sizeof(uint32_t) <= len - index) {
    uint32_t value = tgos_ivc_to_le32(tgos_ivc_fill_next_word(&state));

    memcpy(dst + index, &value, sizeof(value));
    index += sizeof(value);
  }
  if (index < len) {
    uint32_t value = tgos_ivc_fill_next_word(&state);

    for (size_t byte = 0U; index + byte < len; byte++) {
      dst[index + byte] = (uint8_t)(value >> (byte * 8U));
    }
  }
}

#if AXIVC_BENCH_PREPARED
static inline void tgos_ivc_prepared_prepare(uint8_t *buffer) {
  for (uint32_t offset = 0U; offset < TGOS_IVC_PREPARED_MAX_BYTES;
       offset += TGOS_IVC_CHUNK_SIZE) {
    size_t chunk_len = TGOS_IVC_PREPARED_MAX_BYTES - offset;

    if (chunk_len > TGOS_IVC_CHUNK_SIZE) {
      chunk_len = TGOS_IVC_CHUNK_SIZE;
    }
    tgos_ivc_fill_chunk(buffer + offset, chunk_len, 0U, 0U, offset);
  }
}

static inline void tgos_ivc_prepared_mark(uint8_t *buffer, uint32_t case_index,
                                          uint32_t iteration,
                                          size_t payload_len) {
  uint32_t marker[TGOS_IVC_PREPARED_MARKER_BYTES / sizeof(uint32_t)] = {
      tgos_ivc_to_le32(TGOS_IVC_PREPARED_MARKER_MAGIC),
      tgos_ivc_to_le32(case_index),
      tgos_ivc_to_le32(iteration),
      tgos_ivc_to_le32((uint32_t)payload_len),
  };

  memcpy(buffer, marker, sizeof(marker));
}

static inline void tgos_ivc_prepared_add_word64(uint64_t word, uint64_t *sum) {
  word = tgos_ivc_to_le64(word);
  *sum += (uint64_t)(uint32_t)word + (uint32_t)(word >> 32U);
}

static inline uint64_t tgos_ivc_prepared_checksum(const uint8_t *data,
                                                  size_t len) {
  size_t offset = 0U;
  uint64_t sum0 = 0U;
  uint64_t sum1 = 0U;
  uint64_t sum2 = 0U;
  uint64_t sum3 = 0U;

  for (; offset + 32U <= len; offset += 32U) {
    uint64_t word0;
    uint64_t word1;
    uint64_t word2;
    uint64_t word3;

    memcpy(&word0, data + offset + 0U, sizeof(word0));
    memcpy(&word1, data + offset + 8U, sizeof(word1));
    memcpy(&word2, data + offset + 16U, sizeof(word2));
    memcpy(&word3, data + offset + 24U, sizeof(word3));
    tgos_ivc_prepared_add_word64(word0, &sum0);
    tgos_ivc_prepared_add_word64(word1, &sum1);
    tgos_ivc_prepared_add_word64(word2, &sum2);
    tgos_ivc_prepared_add_word64(word3, &sum3);
  }
  for (; offset + sizeof(uint32_t) <= len; offset += sizeof(uint32_t)) {
    uint32_t word;

    memcpy(&word, data + offset, sizeof(word));
    sum0 += tgos_ivc_to_le32(word);
  }
  for (; offset < len; offset++) {
    sum0 += data[offset];
  }
  return sum0 + sum1 + sum2 + sum3;
}
#endif

/*
 * Word-oriented FNV-style checksum. The application only updates this over
 * 4 KiB chunks, keeping every intermediate length divisible by eight.
 */
static inline uint64_t
tgos_ivc_checksum_update(uint64_t checksum, const uint8_t *data, size_t len) {
  size_t index = 0U;

  while (sizeof(uint64_t) <= len - index) {
    uint64_t word;

    memcpy(&word, data + index, sizeof(word));
    word = tgos_ivc_to_le64(word);
    checksum ^= word;
    checksum *= 1099511628211ULL;
    index += sizeof(word);
  }
  while (index < len) {
    checksum ^= data[index++];
    checksum *= 1099511628211ULL;
  }
  return checksum;
}

static inline uint64_t tgos_ivc_checksum_begin(void) {
  return 14695981039346656037ULL;
}

/*
 * Rebuild the expected chunk while checksumming both the received bytes and the
 * generated bytes. Payload verification therefore checks the wire data itself,
 * not only a checksum supplied in the request frame.
 */
static inline int
tgos_ivc_verify_chunk(const uint8_t *actual, uint8_t *expected, size_t len,
                      uint32_t case_index, uint32_t iteration, uint32_t offset,
                      uint64_t *actual_checksum, uint64_t *expected_checksum) {
  tgos_ivc_fill_chunk(expected, len, case_index, iteration, offset);
  *actual_checksum = tgos_ivc_checksum_update(*actual_checksum, actual, len);
  *expected_checksum =
      tgos_ivc_checksum_update(*expected_checksum, expected, len);
  return memcmp(actual, expected, len) == 0 ? 0 : -1;
}

static inline void tgos_ivc_frame_init(struct tgos_ivc_frame *frame,
                                       uint8_t type, uint32_t case_index,
                                       uint32_t iteration, size_t payload_len,
                                       uint64_t checksum, uint64_t total_bytes,
                                       uint64_t value) {
  frame->magic = TGOS_IVC_FRAME_MAGIC;
  frame->version = TGOS_IVC_FRAME_VERSION;
  frame->type = type;
  frame->flags = 0U;
  frame->case_index = case_index;
  frame->iteration = iteration;
  frame->payload_len = (uint32_t)payload_len;
  frame->chunk_size = TGOS_IVC_CHUNK_SIZE;
  frame->checksum = checksum;
  frame->total_bytes = total_bytes;
  frame->value = value;
}

static inline int tgos_ivc_frame_is_valid(const struct tgos_ivc_frame *frame) {
#if AXIVC_BENCH_PREPARED
  uint8_t flags_valid =
      frame->flags == 0U || frame->flags == TGOS_IVC_FRAME_FLAG_PREPARED;
#else
  uint8_t flags_valid = frame->flags == 0U;
#endif

  return frame->magic == TGOS_IVC_FRAME_MAGIC &&
         frame->version == TGOS_IVC_FRAME_VERSION && flags_valid &&
         frame->chunk_size == TGOS_IVC_CHUNK_SIZE;
}

/*
 * An ACK carries the type of the frame it acknowledges in `value`. Matching
 * every request field prevents a delayed ACK from releasing a later payload.
 */
static inline int tgos_ivc_ack_matches(const struct tgos_ivc_frame *request,
                                       const struct tgos_ivc_frame *ack) {
  return tgos_ivc_frame_is_valid(ack) && ack->type == TGOS_IVC_FRAME_ACK &&
         ack->flags == request->flags && ack->value == request->type &&
         ack->case_index == request->case_index &&
         ack->iteration == request->iteration &&
         ack->payload_len == request->payload_len &&
         ack->checksum == request->checksum &&
         ack->total_bytes == request->total_bytes;
}

static inline void tgos_ivc_ack_init(struct tgos_ivc_frame *ack,
                                     const struct tgos_ivc_frame *request) {
  tgos_ivc_frame_init(ack, TGOS_IVC_FRAME_ACK, request->case_index,
                      request->iteration, request->payload_len,
                      request->checksum, request->total_bytes, request->type);
  ack->flags = request->flags;
}

static inline uint64_t tgos_ivc_bandwidth_centi_mib(uint64_t elapsed_us,
                                                    uint64_t bytes) {
  if (elapsed_us == 0U) {
    return 0U;
  }
  return (bytes * 100000000ULL) / (elapsed_us * 1024ULL * 1024ULL);
}

#endif /* TGOS_IVC_APP_H */
