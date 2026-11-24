/* SPDX-License-Identifier: Apache-2.0 */

#include "starry_axivc.h"

#include <errno.h>
#include <stdint.h>
#include <stdio.h>

struct bench_state {
  uint32_t case_index;
  uint32_t iteration;
  uint32_t completed_cases;
  uint64_t total_bytes;
  uint64_t case_bandwidth_sum;
};

static void bench_fail(struct starry_axivc *ivc, struct bench_state *state,
                       const char *reason, const struct tgos_ivc_frame *frame,
                       int error) {
  struct tgos_ivc_frame failure;
  uint32_t case_index = frame != NULL ? frame->case_index : state->case_index;
  uint32_t iteration = frame != NULL ? frame->iteration : state->iteration;
  int error_code = error < 0 ? -error : EIO;

  if (ivc != NULL && ivc->fd >= 0) {
    tgos_ivc_frame_init(&failure, TGOS_IVC_FRAME_FAIL, case_index, iteration,
                        0U, 0U, state->total_bytes, (uint64_t)error_code);
    (void)starry_axivc_send_frame(ivc, &failure, 100);
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

static int verify_payload(const struct starry_axivc *ivc,
                          const struct tgos_ivc_frame *frame,
                          uint32_t *mismatch_offset) {
  uint8_t expected[TGOS_IVC_CHUNK_SIZE];
  uint64_t actual_checksum = tgos_ivc_checksum_begin();
  uint64_t expected_checksum = tgos_ivc_checksum_begin();

  for (uint32_t offset = 0U; offset < frame->payload_len;
       offset += TGOS_IVC_CHUNK_SIZE) {
    size_t chunk_len = frame->payload_len - offset;

    if (chunk_len > TGOS_IVC_CHUNK_SIZE) {
      chunk_len = TGOS_IVC_CHUNK_SIZE;
    }
    if (tgos_ivc_verify_chunk(ivc->peer_payload + offset, expected, chunk_len,
                              frame->case_index, frame->iteration, offset,
                              &actual_checksum, &expected_checksum) != 0) {
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

static void log_stats(uint32_t case_index, uint32_t datasize,
                      uint64_t send_bandwidth, uint64_t receive_bandwidth) {
  printf("[test_output] average sendBandwidth = %llu.%02llu MB/s, "
         "average receiveBandwidth = %llu.%02llu MB/s, testTime = %u, "
         "datasize = %u\n",
         (unsigned long long)(send_bandwidth / 100U),
         (unsigned long long)(send_bandwidth % 100U),
         (unsigned long long)(receive_bandwidth / 100U),
         (unsigned long long)(receive_bandwidth % 100U),
         (unsigned int)AXIVC_BENCH_ITERATIONS, datasize);
  printf("ivc bench receive case=%u datasize=%u\n", case_index, datasize);
}

static int send_ack(struct starry_axivc *ivc,
                    const struct tgos_ivc_frame *request) {
  struct tgos_ivc_frame ack;

  tgos_ivc_ack_init(&ack, request);
  return starry_axivc_send_frame(ivc, &ack, TGOS_IVC_TIMEOUT_MS);
}

int main(void) {
  struct starry_axivc ivc;
  struct bench_state state = {0};
  int result;

  setvbuf(stdout, NULL, _IOLBF, 0);
  result = starry_axivc_open(
      &ivc, tgos_ivc_bench_size_at(TGOS_IVC_BENCH_CASE_COUNT - 1U));
  if (result != 0) {
    bench_fail(NULL, &state, "open", NULL, result);
    return 1;
  }

  printf("AXIVC Starry benchmark ready device=%s generation=%llu capacity=%zu "
         "local_offset=%llu peer_offset=%llu iterations=%u\n",
         TGOS_STARRY_AXIVC_DEVICE, (unsigned long long)ivc.info.generation,
         ivc.capacity, (unsigned long long)ivc.info.local_offset,
         (unsigned long long)ivc.info.peer_offset,
         (unsigned int)AXIVC_BENCH_ITERATIONS);

  for (;;) {
    struct tgos_ivc_frame frame;

    result = starry_axivc_recv_frame(&ivc, &frame, TGOS_IVC_TIMEOUT_MS);
    if (result != 0) {
      bench_fail(&ivc, &state, "receive", NULL, result);
      goto out;
    }
    if (!tgos_ivc_frame_is_valid(&frame)) {
      bench_fail(&ivc, &state, "frame", &frame, -EPROTO);
      goto out;
    }

    switch (frame.type) {
    case TGOS_IVC_FRAME_DATA: {
      uint32_t mismatch_offset = 0U;
      uint64_t start;
      uint64_t elapsed;

      if (state.case_index >= TGOS_IVC_BENCH_CASE_COUNT) {
        bench_fail(&ivc, &state, "unexpected-data", &frame, -EPROTO);
        goto out;
      }
      if (frame.case_index != state.case_index ||
          frame.iteration != state.iteration ||
          frame.payload_len != tgos_ivc_bench_size_at(state.case_index)) {
        bench_fail(&ivc, &state, "sequence", &frame, -EPROTO);
        goto out;
      }

      start = starry_now_us();
      result = verify_payload(&ivc, &frame, &mismatch_offset);
      elapsed = starry_now_us() - start;
      if (result != 0) {
        fprintf(stderr,
                "AXIVC Starry payload mismatch case=%u "
                "iteration=%u offset=%u\n",
                frame.case_index, frame.iteration, mismatch_offset);
        bench_fail(&ivc, &state, "checksum", &frame, result);
        goto out;
      }
      state.total_bytes += frame.payload_len;
      if (frame.total_bytes != state.total_bytes) {
        bench_fail(&ivc, &state, "total-bytes", &frame, -EPROTO);
        goto out;
      }
      state.case_bandwidth_sum +=
          tgos_ivc_bandwidth_centi_mib(elapsed, frame.payload_len);
      state.iteration++;

      result = send_ack(&ivc, &frame);
      if (result != 0) {
        bench_fail(&ivc, &state, "ack-data", &frame, result);
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
        bench_fail(&ivc, &state, "stats", &frame, -EPROTO);
        goto out;
      }

      receive_bandwidth =
          state.case_bandwidth_sum / (uint64_t)AXIVC_BENCH_ITERATIONS;
      log_stats(state.case_index, datasize, frame.value, receive_bandwidth);
      result = send_ack(&ivc, &frame);
      if (result != 0) {
        bench_fail(&ivc, &state, "ack-stats", &frame, result);
        goto out;
      }
      state.completed_cases++;
      state.case_index++;
      state.iteration = 0U;
      state.case_bandwidth_sum = 0U;
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
        bench_fail(&ivc, &state, "done", &frame, -EPROTO);
        goto out;
      }
      result = send_ack(&ivc, &frame);
      if (result != 0) {
        bench_fail(&ivc, &state, "ack-done", &frame, result);
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
      bench_fail(&ivc, &state, "peer-fail", &frame, -(int)frame.value);
      goto out;
    default:
      bench_fail(&ivc, &state, "unexpected-type", &frame, -EPROTO);
      goto out;
    }
  }

out:
  starry_axivc_close(&ivc);
  return result == 0 ? 0 : 1;
}
