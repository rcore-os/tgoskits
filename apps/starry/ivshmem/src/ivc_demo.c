/* SPDX-License-Identifier: Apache-2.0 */

#include "starry_axivc.h"

#include <errno.h>
#include <stdint.h>
#include <stdio.h>

static void demo_fail(const char *reason, const struct tgos_ivc_frame *frame,
                      int error) {
  if (frame != NULL) {
    fprintf(stderr,
            "AXIVC Starry peer failed reason=%s case=%u iteration=%u "
            "error=%s\n",
            reason, frame->case_index, frame->iteration,
            starry_error_name(error));
  } else {
    fprintf(stderr, "AXIVC Starry peer failed reason=%s error=%s\n", reason,
            starry_error_name(error));
  }
  fflush(stderr);
}

static int verify_payload(const struct starry_axivc_payload_view *payload,
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
    if (tgos_ivc_verify_chunk(payload->peer_payload + offset, expected,
                              chunk_len, frame->case_index, frame->iteration,
                              offset, &actual_checksum,
                              &expected_checksum) != 0) {
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

int main(void) {
  struct starry_axivc_payload_view payload;
  struct ivc *connection = NULL;
  uint32_t next_case = 0U;
  int result;

  setvbuf(stdout, NULL, _IOLBF, 0);
  result = ivc_open(&connection);
  if (result != 0) {
    demo_fail("open", NULL, result);
    return 1;
  }
  result = starry_axivc_payload_get(
      connection, tgos_ivc_demo_size_at(TGOS_IVC_DEMO_CASE_COUNT - 1U),
      &payload);
  if (result != 0) {
    demo_fail("payload", NULL, result);
    goto out;
  }

  printf("AXIVC Starry peer ready device=%s generation=%llu capacity=%zu "
         "local_offset=%llu peer_offset=%llu\n",
         TGOS_STARRY_AXIVC_DEVICE, (unsigned long long)payload.generation,
         payload.capacity, (unsigned long long)payload.local_offset,
         (unsigned long long)payload.peer_offset);

  while (next_case < TGOS_IVC_DEMO_CASE_COUNT) {
    struct tgos_ivc_frame frame;
    struct tgos_ivc_frame ack;
    int received;
    uint32_t mismatch_offset = 0U;
    size_t expected_len = tgos_ivc_demo_size_at(next_case);

    if (next_case == 0U) {
      uint8_t short_buffer[IVC_MAX_RECORD_BYTES - 1U];

      received = ivc_recv(connection, short_buffer, sizeof(short_buffer),
                          TGOS_IVC_TIMEOUT_MS);
      if (received != -ENOSPC) {
        result = received > 0 ? -EPROTO : received;
        demo_fail("short-buffer", NULL, result);
        goto out;
      }
    }

    received =
        ivc_recv(connection, &frame, sizeof(frame), TGOS_IVC_TIMEOUT_MS);
    if (received != (int)sizeof(frame)) {
      result = received > 0 ? -EPROTO : received;
      demo_fail(received > 0 ? "frame-length" : "receive", NULL, result);
      goto out;
    }
    result = 0;
    if (!tgos_ivc_frame_is_valid(&frame) || frame.type != TGOS_IVC_FRAME_DATA) {
      demo_fail("frame", &frame, -EPROTO);
      goto out;
    }
    if (frame.case_index != next_case || frame.iteration != 0U ||
        frame.payload_len != expected_len) {
      demo_fail("sequence", &frame, -EPROTO);
      goto out;
    }

    result = verify_payload(&payload, &frame, &mismatch_offset);
    if (result != 0) {
      fprintf(stderr,
              "AXIVC Starry payload mismatch case=%u iteration=%u "
              "offset=%u\n",
              frame.case_index, frame.iteration, mismatch_offset);
      demo_fail("checksum", &frame, result);
      goto out;
    }

    tgos_ivc_ack_init(&ack, &frame);
    result = ivc_send(connection, &ack, sizeof(ack), TGOS_IVC_TIMEOUT_MS);
    if (result != 0) {
      demo_fail("ack", &frame, result);
      goto out;
    }
    printf("AXIVC Starry verified case=%u bytes=%u checksum=%016llx\n",
           frame.case_index, frame.payload_len,
           (unsigned long long)frame.checksum);
    next_case++;
  }

  printf("AXIVC Starry peer pass\n");
out:
  {
    int close_result = ivc_close(connection);

    if (close_result != 0 && result == 0) {
      result = close_result;
      demo_fail("close", NULL, result);
    }
  }
  return result == 0 ? 0 : 1;
}
