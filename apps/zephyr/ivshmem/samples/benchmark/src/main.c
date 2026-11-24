/* SPDX-License-Identifier: Apache-2.0 */

#include "tgos_ivc_app.h"

#include <errno.h>
#include <stdint.h>

#include <tgos_ivc.h>
#include <zephyr/kernel.h>

struct producer_state {
  uint64_t total_bytes;
  uint32_t case_index;
  uint32_t iteration;
};

static void bench_fail(struct producer_state *state, const char *reason,
                       const struct tgos_ivc_frame *frame, int error) {
  uint32_t case_index = frame != NULL ? frame->case_index : state->case_index;
  uint32_t iteration = frame != NULL ? frame->iteration : state->iteration;

  printk("AXVISOR_IVC_BENCH_FAIL reason=%s case=%u iteration=%u "
         "bytes=%llu error=%d\n",
         reason, case_index, iteration, (unsigned long long)state->total_bytes,
         error);
  printk("AXVISOR_IVC_BENCH_RESULT=FAIL reason=%s case=%u iteration=%u "
         "bytes=%llu error=%d\n",
         reason, case_index, iteration, (unsigned long long)state->total_bytes,
         error);
}

static int generate_and_publish(uint32_t case_index, uint32_t iteration,
                                size_t len, uint64_t *checksum,
                                uint64_t *elapsed_us) {
  uint8_t chunk[TGOS_IVC_CHUNK_SIZE];
  uint64_t sum = tgos_ivc_checksum_begin();
  uint64_t start = k_cyc_to_us_floor64(k_cycle_get_64());

  for (size_t offset = 0U; offset < len; offset += TGOS_IVC_CHUNK_SIZE) {
    size_t chunk_len = len - offset;
    int result;

    if (chunk_len > TGOS_IVC_CHUNK_SIZE) {
      chunk_len = TGOS_IVC_CHUNK_SIZE;
    }
    tgos_ivc_fill_chunk(chunk, chunk_len, case_index, iteration,
                        (uint32_t)offset);
    sum = tgos_ivc_checksum_update(sum, chunk, chunk_len);
    result = tgos_ivc_write_payload(offset, chunk, chunk_len);
    if (result != 0) {
      return result < 0 ? result : -EIO;
    }
  }
  *checksum = sum;
  *elapsed_us = k_cyc_to_us_floor64(k_cycle_get_64()) - start;
  return 0;
}

static int receive_ack(const struct tgos_ivc_frame *request) {
  struct tgos_ivc_frame ack;
  int received = tgos_ivc_recv(&ack, sizeof(ack), TGOS_IVC_TIMEOUT_MS);

  if (received < 0) {
    return received;
  }
  if ((size_t)received != sizeof(ack)) {
    printk("AXIVC Zephyr short frame read bytes=%d expected=%zu\n", received,
           sizeof(ack));
    return -EMSGSIZE;
  }
  if (ack.type == TGOS_IVC_FRAME_FAIL) {
    return -(int)ack.value;
  }
  if (!tgos_ivc_ack_matches(request, &ack)) {
    return -EPROTO;
  }
  return 0;
}

static int send_and_wait(const struct tgos_ivc_frame *frame) {
  int result = tgos_ivc_send(frame, sizeof(*frame), TGOS_IVC_TIMEOUT_MS);

  if (result != 0) {
    return result;
  }
  return receive_ack(frame);
}

int main(void) {
  struct producer_state state = {0};
  int result;

  result = tgos_ivc_open();
  if (result != 0) {
    bench_fail(&state, "open", NULL, result);
    return 1;
  }
  result = tgos_ivc_wait_ready(TGOS_IVC_TIMEOUT_MS);
  if (result != 0) {
    bench_fail(&state, "ready", NULL, result);
    tgos_ivc_close();
    return 1;
  }
  if (tgos_ivc_payload_capacity() <
      tgos_ivc_bench_size_at(TGOS_IVC_BENCH_CASE_COUNT - 1U)) {
    bench_fail(&state, "capacity", NULL, -ENOSPC);
    tgos_ivc_close();
    return 1;
  }

  printk("AXIVC Zephyr-Starry benchmark ready iterations=%u capacity=%zu\n",
         (unsigned int)AXIVC_BENCH_ITERATIONS, tgos_ivc_payload_capacity());

  for (state.case_index = 0U; state.case_index < TGOS_IVC_BENCH_CASE_COUNT;
       state.case_index++) {
    size_t datasize = tgos_ivc_bench_size_at(state.case_index);
    uint64_t bandwidth_sum = 0U;

    for (state.iteration = 0U; state.iteration < AXIVC_BENCH_ITERATIONS;
         state.iteration++) {
      struct tgos_ivc_frame frame;
      uint64_t checksum = 0U;
      uint64_t elapsed_us = 0U;

      result = generate_and_publish(state.case_index, state.iteration, datasize,
                                    &checksum, &elapsed_us);
      if (result != 0) {
        bench_fail(&state, "payload-write", NULL, result);
        goto out;
      }

      state.total_bytes += datasize;
      tgos_ivc_frame_init(&frame, TGOS_IVC_FRAME_DATA, state.case_index,
                          state.iteration, datasize, checksum,
                          state.total_bytes, elapsed_us);
      result = send_and_wait(&frame);
      if (result != 0) {
        bench_fail(&state, "data-ack", &frame, result);
        goto out;
      }
      bandwidth_sum += tgos_ivc_bandwidth_centi_mib(elapsed_us, datasize);
    }

    struct tgos_ivc_frame stats;
    uint64_t average_bandwidth =
        bandwidth_sum / (uint64_t)AXIVC_BENCH_ITERATIONS;

    tgos_ivc_frame_init(&stats, TGOS_IVC_FRAME_STATS, state.case_index, 0U,
                        datasize, 0U, state.total_bytes, average_bandwidth);
    result = send_and_wait(&stats);
    if (result != 0) {
      bench_fail(&state, "stats-ack", &stats, result);
      goto out;
    }
    printk("ivc bench publish case=%u datasize=%zu\n", state.case_index,
           datasize);
  }

  struct tgos_ivc_frame done;

  tgos_ivc_frame_init(&done, TGOS_IVC_FRAME_DONE, TGOS_IVC_BENCH_CASE_COUNT,
                      AXIVC_BENCH_ITERATIONS, TGOS_IVC_BENCH_CASE_COUNT, 0U,
                      tgos_ivc_bench_total_bytes(), AXIVC_BENCH_ITERATIONS);
  result = send_and_wait(&done);
  if (result != 0) {
    bench_fail(&state, "done-ack", &done, result);
    goto out;
  }

  printk("AXIVC Zephyr benchmark producer pass\n");
  result = 0;
out:
  tgos_ivc_close();
  return result == 0 ? 0 : 1;
}
