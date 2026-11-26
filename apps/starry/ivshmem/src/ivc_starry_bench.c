/* SPDX-License-Identifier: Apache-2.0 */

#include "starry_axivc.h"

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#if AXIVC_BENCH_PREPARED
static _Alignas(uint64_t) uint8_t
    prepared_expected[TGOS_IVC_PREPARED_MAX_BYTES];
#endif

struct bench_state {
  uint32_t case_index;
  uint32_t iteration;
  uint32_t completed_cases;
  uint64_t total_bytes;
  uint64_t case_bandwidth_sum;
  uint64_t case_bar2_copy_us;
  uint64_t case_local_verify_us;
};

static void bench_fail(struct ivc *connection, struct bench_state *state,
                       const char *reason, const struct tgos_ivc_frame *frame,
                       int error) {
  struct tgos_ivc_frame failure;
  uint32_t case_index = frame != NULL ? frame->case_index : state->case_index;
  uint32_t iteration = frame != NULL ? frame->iteration : state->iteration;
  int error_code = error < 0 ? -error : EIO;

  if (connection != NULL) {
    tgos_ivc_frame_init(&failure, TGOS_IVC_FRAME_FAIL, case_index, iteration,
                        0U, 0U, state->total_bytes, (uint64_t)error_code);
    (void)ivc_send(connection, &failure, sizeof(failure), 100);
  }
  fprintf(stderr,
          "AXVISOR_IVC_BENCH_FAIL reason=%s case=%u iteration=%u "
          "bytes=%llu error=%s\n",
          reason, case_index, iteration, (unsigned long long)state->total_bytes,
          starry_error_name(error));
  printf("AXVISOR_IVC_BENCH_RESULT=FAIL reason=%s case=%u iteration=%u "
         "bytes=%llu error=%s\n",
         reason, case_index, iteration, (unsigned long long)state->total_bytes,
         starry_error_name(error));
  fflush(stdout);
  fflush(stderr);
}

#if AXIVC_BENCH_PREPARED
/*
 * Check the prepared frame by reading each peer byte once. Wide alias-safe
 * loads feed both the exact byte comparison and the 32-bit word sum. Cache
 * invalidation is intentionally outside this measurement.
 */
static int prepared_checked_read(const uint8_t *actual, const uint8_t *expected,
                                 size_t len, uint64_t *checksum_out) {
  uint64_t sum0 = 0U;
  uint64_t sum1 = 0U;
  uint64_t sum2 = 0U;
  uint64_t sum3 = 0U;
  uint64_t mismatch = 0U;
  size_t offset = 0U;

  for (; offset + 32U <= len; offset += 32U) {
    uint64_t actual0;
    uint64_t actual1;
    uint64_t actual2;
    uint64_t actual3;
    uint64_t expected0;
    uint64_t expected1;
    uint64_t expected2;
    uint64_t expected3;

    memcpy(&actual0, actual + offset + 0U, sizeof(actual0));
    memcpy(&actual1, actual + offset + 8U, sizeof(actual1));
    memcpy(&actual2, actual + offset + 16U, sizeof(actual2));
    memcpy(&actual3, actual + offset + 24U, sizeof(actual3));
    memcpy(&expected0, expected + offset + 0U, sizeof(expected0));
    memcpy(&expected1, expected + offset + 8U, sizeof(expected1));
    memcpy(&expected2, expected + offset + 16U, sizeof(expected2));
    memcpy(&expected3, expected + offset + 24U, sizeof(expected3));

    mismatch |= (actual0 ^ expected0) | (actual1 ^ expected1) |
                (actual2 ^ expected2) | (actual3 ^ expected3);
    sum0 += (uint64_t)(uint32_t)actual0 + (uint32_t)(actual0 >> 32U);
    sum1 += (uint64_t)(uint32_t)actual1 + (uint32_t)(actual1 >> 32U);
    sum2 += (uint64_t)(uint32_t)actual2 + (uint32_t)(actual2 >> 32U);
    sum3 += (uint64_t)(uint32_t)actual3 + (uint32_t)(actual3 >> 32U);
  }
  for (; offset + sizeof(uint32_t) <= len; offset += sizeof(uint32_t)) {
    uint32_t actual_word;
    uint32_t expected_word;

    memcpy(&actual_word, actual + offset, sizeof(actual_word));
    memcpy(&expected_word, expected + offset, sizeof(expected_word));
    mismatch |= actual_word ^ expected_word;
    sum0 += actual_word;
  }
  for (; offset < len; offset++) {
    mismatch |= actual[offset] ^ expected[offset];
    sum0 += actual[offset];
  }

  *checksum_out = sum0 + sum1 + sum2 + sum3;
  return mismatch == 0U;
}

#endif

#if !AXIVC_BENCH_PREPARED
static int verify_payload(const struct starry_axivc_payload_view *payload,
                          const struct tgos_ivc_frame *frame,
                          uint32_t *mismatch_offset, uint64_t *bar2_copy_us,
                          uint64_t *local_verify_us) {
  uint8_t actual[TGOS_IVC_CHUNK_SIZE];
  uint8_t expected[TGOS_IVC_CHUNK_SIZE];
  uint64_t actual_checksum = tgos_ivc_checksum_begin();
  uint64_t expected_checksum = tgos_ivc_checksum_begin();
  uint64_t start;
  int verify_result;

  for (uint32_t offset = 0U; offset < frame->payload_len;
       offset += TGOS_IVC_CHUNK_SIZE) {
    size_t chunk_len = frame->payload_len - offset;

    if (chunk_len > TGOS_IVC_CHUNK_SIZE) {
      chunk_len = TGOS_IVC_CHUNK_SIZE;
    }
    /*
     * The DATA notification has been received and the ACK is not sent yet, so
     * the sender cannot reuse the shared payload while this chunk is copied.
     * Read the peer mapping once in bulk, then verify the RAM snapshot.
     */
    start = starry_now_us();
    memcpy(actual, payload->peer_payload + offset, chunk_len);
    *bar2_copy_us += starry_now_us() - start;

    start = starry_now_us();
    verify_result = tgos_ivc_verify_chunk(
        actual, expected, chunk_len, frame->case_index, frame->iteration,
        offset, &actual_checksum, &expected_checksum);
    *local_verify_us += starry_now_us() - start;
    if (verify_result != 0) {
      *mismatch_offset = offset;
      return -EBADMSG;
    }
  }
  if (actual_checksum != expected_checksum ||
      actual_checksum != frame->checksum) {
    *mismatch_offset = UINT32_MAX;
    return -EBADMSG;
  }
  return 0;
}
#endif

static void log_stats(uint32_t case_index, uint32_t datasize,
                      uint64_t send_bandwidth, uint64_t receive_bandwidth,
                      uint64_t bar2_copy_us, uint64_t local_verify_us) {
  (void)bar2_copy_us;
  printf("[test_output] average sendBandwidth = %llu.%02llu MB/s, "
         "average receiveBandwidth = %llu.%02llu MB/s, testTime = %u, "
         "datasize = %u\n",
         (unsigned long long)(send_bandwidth / 100U),
         (unsigned long long)(send_bandwidth % 100U),
         (unsigned long long)(receive_bandwidth / 100U),
         (unsigned long long)(receive_bandwidth % 100U),
         (unsigned int)AXIVC_BENCH_ITERATIONS, datasize);
  printf("ivc bench receive case=%u datasize=%u\n", case_index, datasize);
#if AXIVC_BENCH_PREPARED
  {
    uint64_t total_bytes =
        (uint64_t)datasize * (uint64_t)AXIVC_BENCH_ITERATIONS;
    uint64_t checked_read_mib_s =
        tgos_ivc_bandwidth_centi_mib(local_verify_us, total_bytes);

    printf("AXIVC_IVC_PREPARED_TIMING role=receiver case=%u datasize=%u "
           "iterations=%u direct_peer_checked_read_sum_us=%llu "
           "checked_read_mib_s=%llu.%02llu "
           "checked_read_includes=full_byte_compare_and_32bit_word_sum "
           "cache_invalidate=excluded read_path=direct_peer_mapping "
           "data_generation=excluded clock=guest_monotonic_us "
           "timing_overhead=included_not_subtracted\n",
           case_index, datasize, (unsigned int)AXIVC_BENCH_ITERATIONS,
           (unsigned long long)local_verify_us,
           (unsigned long long)(checked_read_mib_s / 100U),
           (unsigned long long)(checked_read_mib_s % 100U));
  }
#else
  /*
   * These are guest-local monotonic sums. Clock-read and other instrumentation
   * costs are included, not estimated or subtracted, and timestamps are never
   * compared across the two guests.
   */
  printf("AXIVC_IVC_BENCH_TIMING role=receiver case=%u datasize=%u "
         "iterations=%u bar2_copy_sum_us=%llu local_verify_sum_us=%llu "
         "clock=guest_monotonic_us timing_overhead=included_not_subtracted\n",
         case_index, datasize, (unsigned int)AXIVC_BENCH_ITERATIONS,
         (unsigned long long)bar2_copy_us, (unsigned long long)local_verify_us);
#endif
}

static int send_ack(struct ivc *connection,
                    const struct tgos_ivc_frame *request) {
  struct tgos_ivc_frame ack;

  tgos_ivc_ack_init(&ack, request);
  return ivc_send(connection, &ack, sizeof(ack), TGOS_IVC_TIMEOUT_MS);
}

int main(void) {
  struct starry_axivc_payload_view payload;
  struct ivc *connection = NULL;
  struct bench_state state = {0};
  int result;

  setvbuf(stdout, NULL, _IOLBF, 0);
  result = ivc_open(&connection);
  if (result != 0) {
    bench_fail(NULL, &state, "open", NULL, result);
    return 1;
  }
  result = starry_axivc_payload_get(
      connection, tgos_ivc_bench_size_at(TGOS_IVC_BENCH_CASE_COUNT - 1U),
      &payload);
  if (result != 0) {
    bench_fail(connection, &state, "payload", NULL, result);
    goto out;
  }

  printf("AXIVC Starry benchmark ready device=%s generation=%llu capacity=%zu "
         "local_offset=%llu peer_offset=%llu iterations=%u\n",
         TGOS_STARRY_AXIVC_DEVICE, (unsigned long long)payload.generation,
         payload.capacity, (unsigned long long)payload.local_offset,
         (unsigned long long)payload.peer_offset,
         (unsigned int)AXIVC_BENCH_ITERATIONS);
#if AXIVC_BENCH_PREPARED
  tgos_ivc_prepared_prepare(prepared_expected);
#endif

  for (;;) {
    struct tgos_ivc_frame frame;
    int received;

    received =
        ivc_recv(connection, &frame, sizeof(frame), TGOS_IVC_TIMEOUT_MS);
    if (received != (int)sizeof(frame)) {
      result = received > 0 ? -EPROTO : received;
      bench_fail(connection, &state,
                 received > 0 ? "frame-length" : "receive", NULL, result);
      goto out;
    }
    result = 0;
    if (!tgos_ivc_frame_is_valid(&frame)) {
      bench_fail(connection, &state, "frame", &frame, -EPROTO);
      goto out;
    }
#if AXIVC_BENCH_PREPARED
    if (frame.type != TGOS_IVC_FRAME_FAIL &&
        frame.flags != TGOS_IVC_FRAME_FLAG_PREPARED) {
#else
    if (frame.type != TGOS_IVC_FRAME_FAIL && frame.flags != 0U) {
#endif
      bench_fail(connection, &state, "benchmark-mode", &frame, -EPROTO);
      goto out;
    }

    switch (frame.type) {
    case TGOS_IVC_FRAME_DATA: {
      uint64_t bar2_copy_us = 0U;
      uint64_t local_verify_us = 0U;
      uint64_t start;
      uint64_t elapsed;
#if AXIVC_BENCH_PREPARED
      uint64_t actual_checksum;
      int content_matches;
#else
      uint32_t mismatch_offset = 0U;
#endif

      if (state.case_index >= TGOS_IVC_BENCH_CASE_COUNT) {
        bench_fail(connection, &state, "unexpected-data", &frame, -EPROTO);
        goto out;
      }
      if (frame.case_index != state.case_index ||
          frame.iteration != state.iteration ||
          frame.payload_len != tgos_ivc_bench_size_at(state.case_index)) {
        bench_fail(connection, &state, "sequence", &frame, -EPROTO);
        goto out;
      }

#if AXIVC_BENCH_PREPARED
      tgos_ivc_prepared_mark(prepared_expected, frame.case_index,
                             frame.iteration, frame.payload_len);
      start = starry_now_us();
      content_matches =
          prepared_checked_read(payload.peer_payload, prepared_expected,
                                frame.payload_len, &actual_checksum);
      elapsed = starry_now_us() - start;
      local_verify_us = elapsed;
      if (actual_checksum != frame.checksum) {
        bench_fail(connection, &state, "prepared-checksum", &frame, -EBADMSG);
        goto out;
      }
      if (!content_matches) {
        bench_fail(connection, &state, "prepared-content", &frame, -EBADMSG);
        goto out;
      }
#else
      start = starry_now_us();
      result = verify_payload(&payload, &frame, &mismatch_offset,
                              &bar2_copy_us, &local_verify_us);
      elapsed = starry_now_us() - start;
      if (result != 0) {
        fprintf(stderr,
                "AXIVC Starry payload mismatch case=%u "
                "iteration=%u offset=%u\n",
                frame.case_index, frame.iteration, mismatch_offset);
        bench_fail(connection, &state, "checksum", &frame, result);
        goto out;
      }
#endif
      state.case_bar2_copy_us += bar2_copy_us;
      state.case_local_verify_us += local_verify_us;
      state.total_bytes += frame.payload_len;
      if (frame.total_bytes != state.total_bytes) {
        bench_fail(connection, &state, "total-bytes", &frame, -EPROTO);
        goto out;
      }
      state.case_bandwidth_sum +=
          tgos_ivc_bandwidth_centi_mib(elapsed, frame.payload_len);
      state.iteration++;

      result = send_ack(connection, &frame);
      if (result != 0) {
        bench_fail(connection, &state, "ack-data", &frame, result);
        goto out;
      }
      break;
    }
    case TGOS_IVC_FRAME_STATS: {
      uint32_t datasize = (uint32_t)tgos_ivc_bench_size_at(state.case_index);
      uint64_t receive_bandwidth;

      if (state.case_index >= TGOS_IVC_BENCH_CASE_COUNT ||
          state.iteration != AXIVC_BENCH_ITERATIONS ||
          frame.case_index != state.case_index || frame.iteration != 0U ||
          frame.payload_len != datasize || frame.checksum != 0U ||
          frame.total_bytes != state.total_bytes) {
        bench_fail(connection, &state, "stats", &frame, -EPROTO);
        goto out;
      }

      receive_bandwidth =
          state.case_bandwidth_sum / (uint64_t)AXIVC_BENCH_ITERATIONS;
      log_stats(state.case_index, datasize, frame.value, receive_bandwidth,
                state.case_bar2_copy_us, state.case_local_verify_us);
      result = send_ack(connection, &frame);
      if (result != 0) {
        bench_fail(connection, &state, "ack-stats", &frame, result);
        goto out;
      }
      state.completed_cases++;
      state.case_index++;
      state.iteration = 0U;
      state.case_bandwidth_sum = 0U;
      state.case_bar2_copy_us = 0U;
      state.case_local_verify_us = 0U;
      break;
    }
    case TGOS_IVC_FRAME_DONE: {
      uint64_t expected_total = tgos_ivc_bench_total_bytes();

      if (state.completed_cases != TGOS_IVC_BENCH_CASE_COUNT ||
          state.case_index != TGOS_IVC_BENCH_CASE_COUNT ||
          frame.case_index != TGOS_IVC_BENCH_CASE_COUNT ||
          frame.iteration != AXIVC_BENCH_ITERATIONS ||
          frame.payload_len != TGOS_IVC_BENCH_CASE_COUNT ||
          frame.checksum != 0U || frame.total_bytes != expected_total ||
          frame.value != AXIVC_BENCH_ITERATIONS) {
        bench_fail(connection, &state, "done", &frame, -EPROTO);
        goto out;
      }
      result = send_ack(connection, &frame);
      if (result != 0) {
        bench_fail(connection, &state, "ack-done", &frame, result);
        goto out;
      }
      printf("AXVISOR_IVC_BENCH_RESULT=PASS cases=%u testTime=%u "
             "bytes=%llu chunks=%llu\n",
             (unsigned int)TGOS_IVC_BENCH_CASE_COUNT,
             (unsigned int)AXIVC_BENCH_ITERATIONS,
             (unsigned long long)state.total_bytes,
             (unsigned long long)tgos_ivc_bench_total_chunks());
      result = 0;
      goto out;
    }
    case TGOS_IVC_FRAME_FAIL:
      bench_fail(connection, &state, "peer-fail", &frame, -(int)frame.value);
      goto out;
    default:
      bench_fail(connection, &state, "unexpected-type", &frame, -EPROTO);
      goto out;
    }
  }

out:
  {
    int close_result = ivc_close(connection);

    if (close_result != 0 && result == 0) {
      result = close_result;
      bench_fail(NULL, &state, "close", NULL, result);
    }
  }
  return result == 0 ? 0 : 1;
}
