/* SPDX-License-Identifier: Apache-2.0 */
#ifndef AXVISOR_IVSHMEM_IVC_RING_H
#define AXVISOR_IVSHMEM_IVC_RING_H

#include "ivc_msg.h"

#ifdef __cplusplus
extern "C" {
#endif

#define IVC_RING_ALIGN 8U
#define IVC_RING_DEFAULT_SEQ_START 1ULL

enum ivc_peer {
    IVC_PEER_ZEPHYR = 1,
    IVC_PEER_LINUX = 2,
};

enum ivc_status {
    IVC_OK = 0,
    IVC_ERR_INVALID_ARG = -1,
    IVC_ERR_BAD_MAGIC = -2,
    IVC_ERR_BAD_VERSION = -3,
    IVC_ERR_NO_SPACE = -4,
    IVC_ERR_EMPTY = -5,
    IVC_ERR_PAYLOAD_TOO_LARGE = -6,
    IVC_ERR_CHECKSUM = -7,
    IVC_ERR_CORRUPT = -8,
    IVC_ERR_NOT_FOUND = -9,
    IVC_ERR_TIMEOUT = -10,
};

typedef void (*ivc_doorbell_fn)(void *ctx);

struct ivc_ring_view {
    struct ivc_ring_header *header;
    uint8_t *data;
    uint32_t size;
};

struct ivc_endpoint {
    enum ivc_peer peer;
    struct ivc_shared_header *shared;
    struct ivc_ring_view tx;
    struct ivc_ring_view rx;
    uint64_t next_seq;
    ivc_doorbell_fn doorbell;
    void *doorbell_ctx;
};

uint32_t ivc_align_up_u32(uint32_t value, uint32_t align);
uint32_t ivc_checksum32(const void *data, uint32_t len);

int ivc_shared_init(void *bar2, uint32_t total_size, uint32_t z_to_l_size,
                    uint32_t l_to_z_size);
int ivc_endpoint_bind(struct ivc_endpoint *endpoint, void *bar2,
                      uint32_t total_size, enum ivc_peer peer,
                      ivc_doorbell_fn doorbell, void *doorbell_ctx);

int ivc_send(struct ivc_endpoint *endpoint, uint32_t msg_type, uint32_t flags,
             uint64_t reply_to, const void *payload, uint32_t payload_len,
             uint64_t *seq_out);
int ivc_recv(struct ivc_endpoint *endpoint, struct ivc_msg_header *header,
             void *payload, uint32_t payload_capacity,
             uint32_t *payload_len_out);
int ivc_data_alloc(struct ivc_endpoint *endpoint, uint32_t len,
                   uint64_t *offset_out);
int ivc_data_release(struct ivc_endpoint *endpoint, uint64_t offset);
int ivc_data_write(struct ivc_endpoint *endpoint, uint64_t offset,
                   const void *data, uint32_t len);
int ivc_data_read(const struct ivc_endpoint *endpoint, uint64_t offset,
                  void *data, uint32_t len);

static inline int ivc_poll_recv(struct ivc_endpoint *endpoint,
                                struct ivc_msg_header *header, void *payload,
                                uint32_t payload_capacity,
                                uint32_t *payload_len_out)
{
    return ivc_recv(endpoint, header, payload, payload_capacity,
                    payload_len_out);
}

#ifdef __cplusplus
}
#endif

#endif /* AXVISOR_IVSHMEM_IVC_RING_H */
