/* SPDX-License-Identifier: Apache-2.0 */

#include "tgos_ivc_app.h"

#include <errno.h>
#include <stdint.h>

#include <ivc.h>
#include <tgos_ivc.h>
#include <zephyr/kernel.h>

#if AXIVC_BENCH_PREPARED
static _Alignas(uint64_t) uint8_t prepared_source[TGOS_IVC_PREPARED_MAX_BYTES];
#endif

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

#if AXIVC_BENCH_PREPARED
static void log_prepared_sender(uint32_t case_index, size_t datasize,
                                uint64_t write_sum_us,
                                uint64_t data_to_ack_sum_us,
                                uint64_t end_to_end_sum_us) {
  uint64_t total_bytes = (uint64_t)datasize * (uint64_t)AXIVC_BENCH_ITERATIONS;
  uint64_t write_mib_s =
      tgos_ivc_bandwidth_centi_mib(write_sum_us, total_bytes);
  uint64_t data_to_ack_mib_s =
      tgos_ivc_bandwidth_centi_mib(data_to_ack_sum_us, total_bytes);
  uint64_t end_to_end_mib_s =
      tgos_ivc_bandwidth_centi_mib(end_to_end_sum_us, total_bytes);

  printk("AXIVC_IVC_PREPARED_TIMING role=sender case=%u datasize=%zu "
         "iterations=%u write_calls_per_iteration=1 "
         "payload_api=tgos_ivc_write_payload "
         "bulk_write_sum_us=%llu bulk_write_mib_s=%llu.%02llu "
         "data_to_ack_sum_us=%llu data_to_ack_mib_s=%llu.%02llu "
         "end_to_end_sum_us=%llu end_to_end_mib_s=%llu.%02llu "
         "data_generation=excluded end_to_end_scope=bulk_write_plus_data_ack "
         "clock=guest_monotonic_us timing_overhead=included_not_subtracted\n",
         case_index, datasize, (unsigned int)AXIVC_BENCH_ITERATIONS,
         (unsigned long long)write_sum_us,
         (unsigned long long)(write_mib_s / 100U),
         (unsigned long long)(write_mib_s % 100U),
         (unsigned long long)data_to_ack_sum_us,
         (unsigned long long)(data_to_ack_mib_s / 100U),
         (unsigned long long)(data_to_ack_mib_s % 100U),
         (unsigned long long)end_to_end_sum_us,
         (unsigned long long)(end_to_end_mib_s / 100U),
         (unsigned long long)(end_to_end_mib_s % 100U));
}
#endif

#if !AXIVC_BENCH_PREPARED
static int generate_and_publish(uint32_t case_index, uint32_t iteration,
                                size_t len, uint64_t *checksum,
                                uint64_t *generate_us, uint64_t *write_us) {
  uint8_t chunk[TGOS_IVC_CHUNK_SIZE];
  uint64_t sum = tgos_ivc_checksum_begin();

  for (size_t offset = 0U; offset < len; offset += TGOS_IVC_CHUNK_SIZE) {
    size_t chunk_len = len - offset;
    uint64_t start;
    int result;

    if (chunk_len > TGOS_IVC_CHUNK_SIZE) {
      chunk_len = TGOS_IVC_CHUNK_SIZE;
    }
    start = k_cyc_to_us_floor64(k_cycle_get_64());
    tgos_ivc_fill_chunk(chunk, chunk_len, case_index, iteration,
                        (uint32_t)offset);
    sum = tgos_ivc_checksum_update(sum, chunk, chunk_len);
    *generate_us += k_cyc_to_us_floor64(k_cycle_get_64()) - start;

    start = k_cyc_to_us_floor64(k_cycle_get_64());
    result = tgos_ivc_write_payload(offset, chunk, chunk_len);
    *write_us += k_cyc_to_us_floor64(k_cycle_get_64()) - start;
    if (result != 0) {
      return result < 0 ? result : -EIO;
    }
  }
  *checksum = sum;
  return 0;
}
#endif

static int receive_ack(struct ivc *connection,
                       const struct tgos_ivc_frame *request) {
  struct tgos_ivc_frame ack;
  int received = ivc_recv(connection, &ack, sizeof(ack), TGOS_IVC_TIMEOUT_MS);

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

static int send_and_wait(struct ivc *connection,
                         const struct tgos_ivc_frame *frame) {
  int result =
      ivc_send(connection, frame, sizeof(*frame), TGOS_IVC_TIMEOUT_MS);

  if (result != 0) {
    return result;
  }
  return receive_ack(connection, frame);
}

int main(void) {
  struct producer_state state = {0};
  struct ivc *connection = NULL;
  int result;

  result = ivc_open(&connection);
  if (result != 0) {
    bench_fail(&state, "open", NULL, result);
    return 1;
  }
  result = tgos_ivc_wait_ready(TGOS_IVC_TIMEOUT_MS);
  if (result != 0) {
    bench_fail(&state, "ready", NULL, result);
    (void)ivc_close(connection);
    return 1;
  }
  if (tgos_ivc_payload_capacity() <
      tgos_ivc_bench_size_at(TGOS_IVC_BENCH_CASE_COUNT - 1U)) {
    bench_fail(&state, "capacity", NULL, -ENOSPC);
    (void)ivc_close(connection);
    return 1;
  }
#if AXIVC_BENCH_PREPARED
  tgos_ivc_prepared_prepare(prepared_source);
#endif

#if AXIVC_BENCH_PREPARED
  printk("AXIVC Zephyr-Starry benchmark ready iterations=%u prepared=1 "
         "capacity=%zu\n",
         (unsigned int)AXIVC_BENCH_ITERATIONS, tgos_ivc_payload_capacity());
#else
  printk("AXIVC Zephyr-Starry benchmark ready iterations=%u capacity=%zu\n",
         (unsigned int)AXIVC_BENCH_ITERATIONS, tgos_ivc_payload_capacity());
#endif

  for (state.case_index = 0U; state.case_index < TGOS_IVC_BENCH_CASE_COUNT;
       state.case_index++) {
    size_t datasize = tgos_ivc_bench_size_at(state.case_index);
    uint64_t bandwidth_sum = 0U;
    uint64_t write_sum_us = 0U;
    uint64_t data_to_ack_sum_us = 0U;
    uint64_t end_to_end_sum_us = 0U;
#if !AXIVC_BENCH_PREPARED
    uint64_t generate_sum_us = 0U;
#endif

    for (state.iteration = 0U; state.iteration < AXIVC_BENCH_ITERATIONS;
         state.iteration++) {
      struct tgos_ivc_frame frame;
      uint64_t checksum = 0U;
      uint64_t write_us = 0U;
      uint64_t iteration_start_us;
      uint64_t elapsed_us;
      uint64_t ack_start_us;
      uint64_t data_to_ack_us;

#if AXIVC_BENCH_PREPARED
      tgos_ivc_prepared_mark(prepared_source, state.case_index, state.iteration,
                             datasize);
      checksum = tgos_ivc_prepared_checksum(prepared_source, datasize);
      iteration_start_us = k_cyc_to_us_floor64(k_cycle_get_64());
      {
        uint64_t write_start_us = k_cyc_to_us_floor64(k_cycle_get_64());

        result = tgos_ivc_write_payload(0U, prepared_source, datasize);
        write_us = k_cyc_to_us_floor64(k_cycle_get_64()) - write_start_us;
      }
      elapsed_us = k_cyc_to_us_floor64(k_cycle_get_64()) - iteration_start_us;
      if (result != 0) {
        bench_fail(&state, "prepared-payload-write", NULL,
                   result < 0 ? result : -EIO);
        goto out;
      }
#else
      {
        uint64_t generate_us = 0U;

        iteration_start_us = k_cyc_to_us_floor64(k_cycle_get_64());
        result =
            generate_and_publish(state.case_index, state.iteration, datasize,
                                 &checksum, &generate_us, &write_us);
        elapsed_us = k_cyc_to_us_floor64(k_cycle_get_64()) - iteration_start_us;
        generate_sum_us += generate_us;
      }
      if (result != 0) {
        bench_fail(&state, "payload-write", NULL, result);
        goto out;
      }
#endif

      state.total_bytes += datasize;
      tgos_ivc_frame_init(&frame, TGOS_IVC_FRAME_DATA, state.case_index,
                          state.iteration, datasize, checksum,
                          state.total_bytes, elapsed_us);
#if AXIVC_BENCH_PREPARED
      frame.flags = TGOS_IVC_FRAME_FLAG_PREPARED;
#endif
      ack_start_us = k_cyc_to_us_floor64(k_cycle_get_64());
      result = send_and_wait(connection, &frame);
      data_to_ack_us = k_cyc_to_us_floor64(k_cycle_get_64()) - ack_start_us;
      if (result != 0) {
        bench_fail(&state, "data-ack", &frame, result);
        goto out;
      }
      write_sum_us += write_us;
      data_to_ack_sum_us += data_to_ack_us;
      end_to_end_sum_us +=
          k_cyc_to_us_floor64(k_cycle_get_64()) - iteration_start_us;
#if AXIVC_BENCH_PREPARED
      bandwidth_sum += tgos_ivc_bandwidth_centi_mib(write_us, datasize);
#else
      bandwidth_sum += tgos_ivc_bandwidth_centi_mib(elapsed_us, datasize);
#endif
    }

    {
      struct tgos_ivc_frame stats;
      uint64_t average_bandwidth =
          bandwidth_sum / (uint64_t)AXIVC_BENCH_ITERATIONS;

      tgos_ivc_frame_init(&stats, TGOS_IVC_FRAME_STATS, state.case_index, 0U,
                          datasize, 0U, state.total_bytes, average_bandwidth);
#if AXIVC_BENCH_PREPARED
      stats.flags = TGOS_IVC_FRAME_FLAG_PREPARED;
#endif
      result = send_and_wait(connection, &stats);
      if (result != 0) {
        bench_fail(&state, "stats-ack", &stats, result);
        goto out;
      }
    }
    printk("ivc bench publish case=%u datasize=%zu\n", state.case_index,
           datasize);
#if AXIVC_BENCH_PREPARED
    log_prepared_sender(state.case_index, datasize, write_sum_us,
                        data_to_ack_sum_us, end_to_end_sum_us);
#else
    /*
     * These are guest-local monotonic sums. Clock-read and other
     * instrumentation costs are included, not estimated or subtracted, and
     * timestamps are never compared across the two guests.
     */
    printk("AXIVC_IVC_BENCH_TIMING role=sender case=%u datasize=%zu "
           "iterations=%u generate_checksum_sum_us=%llu "
           "bar2_write_sum_us=%llu data_to_ack_sum_us=%llu "
           "end_to_end_sum_us=%llu clock=guest_monotonic_us "
           "timing_overhead=included_not_subtracted\n",
           state.case_index, datasize, (unsigned int)AXIVC_BENCH_ITERATIONS,
           (unsigned long long)generate_sum_us,
           (unsigned long long)write_sum_us,
           (unsigned long long)data_to_ack_sum_us,
           (unsigned long long)end_to_end_sum_us);
#endif
  }

  {
    struct tgos_ivc_frame done;

    tgos_ivc_frame_init(&done, TGOS_IVC_FRAME_DONE, TGOS_IVC_BENCH_CASE_COUNT,
                        AXIVC_BENCH_ITERATIONS, TGOS_IVC_BENCH_CASE_COUNT, 0U,
                        tgos_ivc_bench_total_bytes(), AXIVC_BENCH_ITERATIONS);
#if AXIVC_BENCH_PREPARED
    done.flags = TGOS_IVC_FRAME_FLAG_PREPARED;
#endif
    result = send_and_wait(connection, &done);
    if (result != 0) {
      bench_fail(&state, "done-ack", &done, result);
      goto out;
    }
  }

  printk("AXIVC Zephyr benchmark producer pass\n");
  result = 0;
out:
  (void)ivc_close(connection);
  return result == 0 ? 0 : 1;
}
