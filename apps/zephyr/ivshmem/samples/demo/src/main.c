/* SPDX-License-Identifier: Apache-2.0 */

#include "tgos_ivc_app.h"

#include <errno.h>
#include <stdint.h>

#include <ivc.h>
#include <tgos_ivc.h>
#include <zephyr/kernel.h>

static void demo_fail(const char *reason, const struct tgos_ivc_frame *frame,
                      int error) {
  if (frame != NULL) {
    printk("AXIVC Zephyr demo failed reason=%s case=%u iteration=%u "
           "error=%d\n",
           reason, frame->case_index, frame->iteration, error);
  } else {
    printk("AXIVC Zephyr demo failed reason=%s error=%d\n", reason, error);
  }
}

static int generate_and_publish(uint32_t case_index, uint32_t iteration,
                                size_t len, uint64_t *checksum) {
  uint8_t chunk[TGOS_IVC_CHUNK_SIZE];
  uint64_t sum = tgos_ivc_checksum_begin();

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
  return 0;
}

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
    printk("AXIVC Zephyr peer failure code=%u\n", (unsigned int)ack.value);
    return -(int)ack.value;
  }
  if (!tgos_ivc_ack_matches(request, &ack)) {
    return -EPROTO;
  }
  return 0;
}

int main(void) {
  struct ivc *connection = NULL;
  int result;
  uint32_t completed = 0U;

  result = ivc_open(&connection);
  if (result != 0) {
    demo_fail("open", NULL, result);
    return 1;
  }
  result = tgos_ivc_wait_ready(TGOS_IVC_TIMEOUT_MS);
  if (result != 0) {
    demo_fail("ready", NULL, result);
    (void)ivc_close(connection);
    return 1;
  }
  if (tgos_ivc_payload_capacity() <
      tgos_ivc_demo_size_at(TGOS_IVC_DEMO_CASE_COUNT - 1U)) {
    demo_fail("capacity", NULL, -ENOSPC);
    (void)ivc_close(connection);
    return 1;
  }

  printk("AXIVC Zephyr-Starry ready cases=%u\n",
         (unsigned int)TGOS_IVC_DEMO_CASE_COUNT);
  for (uint32_t case_index = 0U; case_index < TGOS_IVC_DEMO_CASE_COUNT;
       case_index++) {
    struct tgos_ivc_frame frame;
    uint64_t checksum = 0U;
    size_t len = tgos_ivc_demo_size_at(case_index);

    result = generate_and_publish(case_index, 0U, len, &checksum);
    if (result != 0) {
      demo_fail("payload-write", NULL, result);
      goto out;
    }
    tgos_ivc_frame_init(&frame, TGOS_IVC_FRAME_DATA, case_index, 0U, len,
                        checksum, len, 0U);
    result = ivc_send(connection, &frame, sizeof(frame), TGOS_IVC_TIMEOUT_MS);
    if (result != 0) {
      demo_fail("send", &frame, result);
      goto out;
    }
    result = receive_ack(connection, &frame);
    if (result != 0) {
      demo_fail("ack", &frame, result);
      goto out;
    }
    completed++;
    printk("AXIVC Zephyr sent case=%u bytes=%u checksum=%016llx\n", case_index,
           (unsigned int)len, (unsigned long long)checksum);
  }

  if (completed == TGOS_IVC_DEMO_CASE_COUNT) {
    printk("AXIVC Zephyr-Starry pass\n");
    result = 0;
  } else {
    demo_fail("incomplete", NULL, -EPROTO);
    result = -EPROTO;
  }
out:
  (void)ivc_close(connection);
  return result == 0 ? 0 : 1;
}
