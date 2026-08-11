/* SPDX-License-Identifier: Apache-2.0 */
#include "ivc_ring.h"

#include <string.h>

#define IVC_SHARED_MIN_ALIGN 8U

static void ivc_compiler_barrier(void)
{
#if defined(__GNUC__) || defined(__clang__)
    __asm__ __volatile__("" ::: "memory");
#endif
}

uint32_t ivc_align_up_u32(uint32_t value, uint32_t align)
{
    if (align == 0U) {
        return value;
    }
    return (value + align - 1U) & ~(align - 1U);
}

uint32_t ivc_checksum32(const void *data, uint32_t len)
{
    const uint8_t *bytes = (const uint8_t *)data;
    uint32_t hash = 2166136261U;

    if (data == NULL && len != 0U) {
        return 0U;
    }

    for (uint32_t i = 0; i < len; i++) {
        hash ^= bytes[i];
        hash *= 16777619U;
    }
    return hash;
}

static uint64_t ivc_ring_used(const struct ivc_ring_view *ring)
{
    return ring->header->write_pos - ring->header->read_pos;
}

static uint64_t ivc_ring_free(const struct ivc_ring_view *ring)
{
    uint64_t used = ivc_ring_used(ring);
    if (used > ring->size) {
        return 0U;
    }
    return (uint64_t)ring->size - used;
}

static void ivc_ring_copy_in(struct ivc_ring_view *ring, uint64_t pos,
                             const void *src, uint32_t len)
{
    const uint8_t *bytes = (const uint8_t *)src;
    uint32_t offset = (uint32_t)(pos % ring->size);
    uint32_t first = ring->size - offset;

    if (first > len) {
        first = len;
    }
    memcpy(ring->data + offset, bytes, first);
    if (first < len) {
        memcpy(ring->data, bytes + first, len - first);
    }
}

static void ivc_ring_copy_out(const struct ivc_ring_view *ring, uint64_t pos,
                              void *dst, uint32_t len)
{
    uint8_t *bytes = (uint8_t *)dst;
    uint32_t offset = (uint32_t)(pos % ring->size);
    uint32_t first = ring->size - offset;

    if (first > len) {
        first = len;
    }
    memcpy(bytes, ring->data + offset, first);
    if (first < len) {
        memcpy(bytes + first, ring->data, len - first);
    }
}

static void ivc_ring_zero_padding(struct ivc_ring_view *ring, uint64_t pos,
                                  uint32_t len)
{
    uint8_t zeros[IVC_RING_ALIGN] = {0};

    while (len != 0U) {
        uint32_t chunk = len > sizeof(zeros) ? (uint32_t)sizeof(zeros) : len;
        ivc_ring_copy_in(ring, pos, zeros, chunk);
        pos += chunk;
        len -= chunk;
    }
}

static int ivc_ring_init(struct ivc_ring_header *header, uint8_t *data,
                         uint32_t size)
{
    if (header == NULL || data == NULL || size < IVC_MSG_HEADER_SIZE) {
        return IVC_ERR_INVALID_ARG;
    }

    memset(header, 0, sizeof(*header));
    memset(data, 0, size);
    header->magic = IVC_RING_MAGIC;
    header->version = IVC_RING_VERSION;
    header->size = size;
    header->flags = 0;
    header->write_pos = 0;
    header->read_pos = 0;
    return IVC_OK;
}

static struct ivc_data_block_header *
ivc_data_block_at(struct ivc_shared_header *shared, uint64_t offset)
{
    return (struct ivc_data_block_header *)((uint8_t *)shared + offset);
}

static const struct ivc_data_block_header *
ivc_data_block_at_const(const struct ivc_shared_header *shared,
                        uint64_t offset)
{
    return (const struct ivc_data_block_header *)((const uint8_t *)shared +
                                                  offset);
}

static int ivc_data_init(struct ivc_shared_header *shared)
{
    struct ivc_data_block_header *head;

    if (shared->data_size < IVC_DATA_BLOCK_HEADER_SIZE) {
        shared->data_head_offset = 0;
        return IVC_OK;
    }

    shared->data_head_offset = shared->data_offset;
    head = ivc_data_block_at(shared, shared->data_head_offset);
    head->magic = IVC_DATA_BLOCK_MAGIC;
    head->flags = IVC_DATA_BLOCK_FREE;
    head->size = shared->data_size - IVC_DATA_BLOCK_HEADER_SIZE;
    head->next_offset = 0;
    return IVC_OK;
}

static int ivc_ring_validate(const struct ivc_ring_view *ring)
{
    if (ring == NULL || ring->header == NULL || ring->data == NULL ||
        ring->size == 0U) {
        return IVC_ERR_INVALID_ARG;
    }
    if (ring->header->magic != IVC_RING_MAGIC) {
        return IVC_ERR_BAD_MAGIC;
    }
    if (ring->header->version != IVC_RING_VERSION) {
        return IVC_ERR_BAD_VERSION;
    }
    if (ring->header->size != ring->size) {
        return IVC_ERR_CORRUPT;
    }
    if (ivc_ring_used(ring) > ring->size) {
        return IVC_ERR_CORRUPT;
    }
    return IVC_OK;
}

int ivc_shared_init(void *bar2, uint32_t total_size, uint32_t z_to_l_size,
                    uint32_t l_to_z_size)
{
    uint8_t *base = (uint8_t *)bar2;
    struct ivc_shared_header *shared = (struct ivc_shared_header *)bar2;
    uint32_t z_to_l_header_offset;
    uint32_t z_to_l_data_offset;
    uint32_t l_to_z_header_offset;
    uint32_t l_to_z_data_offset;
    uint32_t data_offset;
    uint32_t used;

    if (bar2 == NULL || z_to_l_size < IVC_MSG_HEADER_SIZE ||
        l_to_z_size < IVC_MSG_HEADER_SIZE) {
        return IVC_ERR_INVALID_ARG;
    }

    z_to_l_header_offset =
        ivc_align_up_u32(IVC_SHARED_HEADER_SIZE, IVC_SHARED_MIN_ALIGN);
    z_to_l_data_offset =
        ivc_align_up_u32(z_to_l_header_offset + IVC_RING_HEADER_SIZE,
                         IVC_SHARED_MIN_ALIGN);
    l_to_z_header_offset =
        ivc_align_up_u32(z_to_l_data_offset + z_to_l_size,
                         IVC_SHARED_MIN_ALIGN);
    l_to_z_data_offset =
        ivc_align_up_u32(l_to_z_header_offset + IVC_RING_HEADER_SIZE,
                         IVC_SHARED_MIN_ALIGN);
    data_offset = ivc_align_up_u32(l_to_z_data_offset + l_to_z_size,
                                   IVC_SHARED_MIN_ALIGN);
    used = data_offset;

    if (used > total_size) {
        return IVC_ERR_NO_SPACE;
    }

    memset(base, 0, total_size);
    shared->total_size = total_size;
    shared->flags = 0;
    shared->z_to_l_offset = z_to_l_header_offset;
    shared->z_to_l_size = IVC_RING_HEADER_SIZE + z_to_l_size;
    shared->l_to_z_offset = l_to_z_header_offset;
    shared->l_to_z_size = IVC_RING_HEADER_SIZE + l_to_z_size;
    shared->data_offset = data_offset;
    shared->data_size = total_size - data_offset;
    ivc_data_init(shared);

    ivc_ring_init((struct ivc_ring_header *)(base + z_to_l_header_offset),
                  base + z_to_l_data_offset, z_to_l_size);
    ivc_ring_init((struct ivc_ring_header *)(base + l_to_z_header_offset),
                  base + l_to_z_data_offset, l_to_z_size);
    ivc_compiler_barrier();
    shared->version = IVC_SHARED_VERSION;
    shared->header_len = IVC_SHARED_HEADER_SIZE;
    shared->magic = IVC_SHARED_MAGIC;
    return IVC_OK;
}

static int ivc_view_from_shared(struct ivc_ring_view *view,
                                struct ivc_shared_header *shared,
                                uint64_t offset, uint64_t size)
{
    uint8_t *base = (uint8_t *)shared;

    if (view == NULL || shared == NULL || size < IVC_RING_HEADER_SIZE) {
        return IVC_ERR_INVALID_ARG;
    }
    if (offset + size > shared->total_size) {
        return IVC_ERR_CORRUPT;
    }

    view->header = (struct ivc_ring_header *)(base + offset);
    view->data = base + offset + IVC_RING_HEADER_SIZE;
    view->size = (uint32_t)(size - IVC_RING_HEADER_SIZE);
    return ivc_ring_validate(view);
}

int ivc_endpoint_bind(struct ivc_endpoint *endpoint, void *bar2,
                      uint32_t total_size, enum ivc_peer peer,
                      ivc_doorbell_fn doorbell, void *doorbell_ctx)
{
    struct ivc_shared_header *shared = (struct ivc_shared_header *)bar2;
    struct ivc_ring_view z_to_l;
    struct ivc_ring_view l_to_z;
    int rc;

    if (endpoint == NULL || bar2 == NULL) {
        return IVC_ERR_INVALID_ARG;
    }
    if (shared->magic != IVC_SHARED_MAGIC) {
        return IVC_ERR_BAD_MAGIC;
    }
    if (shared->version != IVC_SHARED_VERSION) {
        return IVC_ERR_BAD_VERSION;
    }
    if (shared->header_len != IVC_SHARED_HEADER_SIZE ||
        shared->total_size > total_size) {
        return IVC_ERR_CORRUPT;
    }

    rc = ivc_view_from_shared(&z_to_l, shared, shared->z_to_l_offset,
                              shared->z_to_l_size);
    if (rc != IVC_OK) {
        return rc;
    }
    rc = ivc_view_from_shared(&l_to_z, shared, shared->l_to_z_offset,
                              shared->l_to_z_size);
    if (rc != IVC_OK) {
        return rc;
    }

    memset(endpoint, 0, sizeof(*endpoint));
    endpoint->peer = peer;
    endpoint->shared = shared;
    endpoint->next_seq = IVC_RING_DEFAULT_SEQ_START;
    endpoint->doorbell = doorbell;
    endpoint->doorbell_ctx = doorbell_ctx;

    if (peer == IVC_PEER_ZEPHYR) {
        endpoint->tx = z_to_l;
        endpoint->rx = l_to_z;
    } else if (peer == IVC_PEER_LINUX) {
        endpoint->tx = l_to_z;
        endpoint->rx = z_to_l;
    } else {
        return IVC_ERR_INVALID_ARG;
    }

    return IVC_OK;
}

int ivc_send(struct ivc_endpoint *endpoint, uint32_t msg_type, uint32_t flags,
             uint64_t reply_to, const void *payload, uint32_t payload_len,
             uint64_t *seq_out)
{
    struct ivc_ring_view *ring;
    struct ivc_msg_header header;
    uint64_t write_pos;
    uint32_t record_len;
    uint32_t padded_len;
    int rc;

    if (endpoint == NULL || (payload == NULL && payload_len != 0U)) {
        return IVC_ERR_INVALID_ARG;
    }

    ring = &endpoint->tx;
    rc = ivc_ring_validate(ring);
    if (rc != IVC_OK) {
        return rc;
    }

    record_len = IVC_MSG_HEADER_SIZE + payload_len;
    padded_len = ivc_align_up_u32(record_len, IVC_RING_ALIGN);
    if (padded_len > ring->size) {
        return IVC_ERR_PAYLOAD_TOO_LARGE;
    }
    if (ivc_ring_free(ring) < padded_len) {
        return IVC_ERR_NO_SPACE;
    }

    memset(&header, 0, sizeof(header));
    header.magic = IVC_MSG_MAGIC;
    header.version = IVC_MSG_VERSION;
    header.header_len = IVC_MSG_HEADER_SIZE;
    header.msg_type = msg_type;
    header.flags = flags;
    header.seq = endpoint->next_seq++;
    header.reply_to = reply_to;
    header.payload_len = payload_len;
    header.checksum = ivc_checksum32(payload, payload_len);
    header.timestamp_ns = 0;

    write_pos = ring->header->write_pos;
    ivc_ring_copy_in(ring, write_pos, &header, IVC_MSG_HEADER_SIZE);
    if (payload_len != 0U) {
        ivc_ring_copy_in(ring, write_pos + IVC_MSG_HEADER_SIZE, payload,
                         payload_len);
    }
    if (padded_len > record_len) {
        ivc_ring_zero_padding(ring, write_pos + record_len,
                              padded_len - record_len);
    }

    ivc_compiler_barrier();
    ring->header->write_pos = write_pos + padded_len;

    if (seq_out != NULL) {
        *seq_out = header.seq;
    }
    if (endpoint->doorbell != NULL) {
        endpoint->doorbell(endpoint->doorbell_ctx);
    }
    return IVC_OK;
}

int ivc_recv(struct ivc_endpoint *endpoint, struct ivc_msg_header *header,
             void *payload, uint32_t payload_capacity,
             uint32_t *payload_len_out)
{
    struct ivc_ring_view *ring;
    uint64_t read_pos;
    uint32_t record_len;
    uint32_t padded_len;
    int rc;

    if (endpoint == NULL || header == NULL) {
        return IVC_ERR_INVALID_ARG;
    }

    ring = &endpoint->rx;
    rc = ivc_ring_validate(ring);
    if (rc != IVC_OK) {
        return rc;
    }
    if (ivc_ring_used(ring) == 0U) {
        return IVC_ERR_EMPTY;
    }
    if (ivc_ring_used(ring) < IVC_MSG_HEADER_SIZE) {
        return IVC_ERR_CORRUPT;
    }

    read_pos = ring->header->read_pos;
    ivc_ring_copy_out(ring, read_pos, header, IVC_MSG_HEADER_SIZE);
    if (header->magic != IVC_MSG_MAGIC) {
        return IVC_ERR_BAD_MAGIC;
    }
    if (header->version != IVC_MSG_VERSION ||
        header->header_len != IVC_MSG_HEADER_SIZE) {
        return IVC_ERR_BAD_VERSION;
    }

    record_len = IVC_MSG_HEADER_SIZE + header->payload_len;
    padded_len = ivc_align_up_u32(record_len, IVC_RING_ALIGN);
    if (padded_len > ring->size || ivc_ring_used(ring) < padded_len) {
        return IVC_ERR_CORRUPT;
    }
    if (header->payload_len > payload_capacity) {
        return IVC_ERR_PAYLOAD_TOO_LARGE;
    }
    if (header->payload_len != 0U && payload == NULL) {
        return IVC_ERR_INVALID_ARG;
    }

    if (header->payload_len != 0U) {
        ivc_ring_copy_out(ring, read_pos + IVC_MSG_HEADER_SIZE, payload,
                          header->payload_len);
        if (ivc_checksum32(payload, header->payload_len) != header->checksum) {
            return IVC_ERR_CHECKSUM;
        }
    } else if (header->checksum != ivc_checksum32(NULL, 0U)) {
        return IVC_ERR_CHECKSUM;
    }

    ivc_compiler_barrier();
    ring->header->read_pos = read_pos + padded_len;

    if (payload_len_out != NULL) {
        *payload_len_out = header->payload_len;
    }
    return IVC_OK;
}

static int ivc_data_validate_shared(const struct ivc_shared_header *shared)
{
    if (shared == NULL || shared->magic != IVC_SHARED_MAGIC ||
        shared->version != IVC_SHARED_VERSION ||
        shared->header_len != IVC_SHARED_HEADER_SIZE) {
        return IVC_ERR_CORRUPT;
    }
    return IVC_OK;
}

static int ivc_data_block_bounds_valid(const struct ivc_shared_header *shared,
                                       uint64_t block_offset)
{
    uint64_t data_end = shared->data_offset + shared->data_size;
    const struct ivc_data_block_header *block;

    if (block_offset < shared->data_offset ||
        block_offset + IVC_DATA_BLOCK_HEADER_SIZE < block_offset ||
        block_offset + IVC_DATA_BLOCK_HEADER_SIZE > data_end) {
        return 0;
    }

    block = ivc_data_block_at_const(shared, block_offset);
    if (block->magic != IVC_DATA_BLOCK_MAGIC ||
        (block->flags != IVC_DATA_BLOCK_FREE &&
         block->flags != IVC_DATA_BLOCK_USED)) {
        return 0;
    }
    if (block_offset + IVC_DATA_BLOCK_HEADER_SIZE + block->size <
            block_offset ||
        block_offset + IVC_DATA_BLOCK_HEADER_SIZE + block->size > data_end) {
        return 0;
    }
    if (block->next_offset != 0 &&
        (block->next_offset <= block_offset ||
         block->next_offset >= data_end)) {
        return 0;
    }
    return 1;
}

static int ivc_data_find_block(const struct ivc_shared_header *shared,
                               uint64_t payload_offset, uint32_t len,
                               uint32_t required_flags,
                               uint64_t *block_offset_out)
{
    uint64_t block_offset;

    if (payload_offset < IVC_DATA_BLOCK_HEADER_SIZE) {
        return IVC_ERR_INVALID_ARG;
    }

    block_offset = shared->data_head_offset;
    while (block_offset != 0) {
        const struct ivc_data_block_header *block;
        uint64_t current_payload;

        if (!ivc_data_block_bounds_valid(shared, block_offset)) {
            return IVC_ERR_CORRUPT;
        }

        block = ivc_data_block_at_const(shared, block_offset);
        current_payload = block_offset + IVC_DATA_BLOCK_HEADER_SIZE;
        if (current_payload == payload_offset) {
            if (block->flags != required_flags) {
                return IVC_ERR_NOT_FOUND;
            }
            if ((uint64_t)len > block->size) {
                return IVC_ERR_INVALID_ARG;
            }
            if (block_offset_out != NULL) {
                *block_offset_out = block_offset;
            }
            return IVC_OK;
        }
        block_offset = block->next_offset;
    }

    return IVC_ERR_NOT_FOUND;
}

static void ivc_data_coalesce_next(struct ivc_shared_header *shared,
                                   uint64_t block_offset)
{
    struct ivc_data_block_header *block = ivc_data_block_at(shared,
                                                            block_offset);
    struct ivc_data_block_header *next;
    uint64_t expected_next = block_offset + IVC_DATA_BLOCK_HEADER_SIZE +
                             block->size;

    if (block->next_offset == 0 || block->next_offset != expected_next ||
        !ivc_data_block_bounds_valid(shared, block->next_offset)) {
        return;
    }

    next = ivc_data_block_at(shared, block->next_offset);
    if (next->flags != IVC_DATA_BLOCK_FREE) {
        return;
    }

    block->size += IVC_DATA_BLOCK_HEADER_SIZE + next->size;
    block->next_offset = next->next_offset;
}

static void ivc_data_coalesce_prev(struct ivc_shared_header *shared,
                                   uint64_t block_offset)
{
    uint64_t current = shared->data_head_offset;

    while (current != 0) {
        struct ivc_data_block_header *block;
        uint64_t expected_next;

        if (!ivc_data_block_bounds_valid(shared, current)) {
            return;
        }
        block = ivc_data_block_at(shared, current);
        if (block->next_offset != block_offset) {
            current = block->next_offset;
            continue;
        }

        expected_next = current + IVC_DATA_BLOCK_HEADER_SIZE + block->size;
        if (block->flags == IVC_DATA_BLOCK_FREE &&
            expected_next == block_offset) {
            ivc_data_coalesce_next(shared, current);
        }
        return;
    }
}

int ivc_data_alloc(struct ivc_endpoint *endpoint, uint32_t len,
                   uint64_t *offset_out)
{
    struct ivc_shared_header *shared;
    uint64_t aligned_len;
    uint64_t block_offset;
    int rc;

    if (endpoint == NULL || endpoint->shared == NULL || offset_out == NULL) {
        return IVC_ERR_INVALID_ARG;
    }
    shared = endpoint->shared;
    rc = ivc_data_validate_shared(shared);
    if (rc != IVC_OK) {
        return rc;
    }

    aligned_len = ivc_align_up_u32(len, IVC_RING_ALIGN);
    block_offset = shared->data_head_offset;
    while (block_offset != 0) {
        struct ivc_data_block_header *block;

        if (!ivc_data_block_bounds_valid(shared, block_offset)) {
            return IVC_ERR_CORRUPT;
        }
        block = ivc_data_block_at(shared, block_offset);
        if (block->flags == IVC_DATA_BLOCK_FREE &&
            block->size >= aligned_len) {
            uint64_t remaining = block->size - aligned_len;

            if (remaining >= IVC_DATA_BLOCK_HEADER_SIZE + IVC_RING_ALIGN) {
                uint64_t next_offset = block_offset +
                                       IVC_DATA_BLOCK_HEADER_SIZE +
                                       aligned_len;
                struct ivc_data_block_header *next =
                    ivc_data_block_at(shared, next_offset);

                next->magic = IVC_DATA_BLOCK_MAGIC;
                next->flags = IVC_DATA_BLOCK_FREE;
                next->size = remaining - IVC_DATA_BLOCK_HEADER_SIZE;
                next->next_offset = block->next_offset;

                block->size = aligned_len;
                block->next_offset = next_offset;
            }

            block->flags = IVC_DATA_BLOCK_USED;
            *offset_out = block_offset + IVC_DATA_BLOCK_HEADER_SIZE;
            return IVC_OK;
        }
        block_offset = block->next_offset;
    }

    return IVC_ERR_NO_SPACE;
}

int ivc_data_release(struct ivc_endpoint *endpoint, uint64_t offset)
{
    struct ivc_shared_header *shared;
    struct ivc_data_block_header *block;
    uint64_t block_offset;
    int rc;

    if (endpoint == NULL || endpoint->shared == NULL) {
        return IVC_ERR_INVALID_ARG;
    }

    shared = endpoint->shared;
    rc = ivc_data_validate_shared(shared);
    if (rc != IVC_OK) {
        return rc;
    }
    rc = ivc_data_find_block(shared, offset, 0, IVC_DATA_BLOCK_USED,
                             &block_offset);
    if (rc != IVC_OK) {
        return rc;
    }

    block = ivc_data_block_at(shared, block_offset);
    block->flags = IVC_DATA_BLOCK_FREE;
    ivc_data_coalesce_next(shared, block_offset);
    ivc_data_coalesce_prev(shared, block_offset);
    return IVC_OK;
}

static int ivc_data_validate_range(const struct ivc_shared_header *shared,
                                   uint64_t offset, uint32_t len)
{
    int rc = ivc_data_validate_shared(shared);

    if (rc != IVC_OK) {
        return rc;
    }
    if (shared->data_head_offset == 0) {
        return IVC_ERR_NO_SPACE;
    }
    return ivc_data_find_block(shared, offset, len, IVC_DATA_BLOCK_USED,
                               NULL);
}

int ivc_data_write(struct ivc_endpoint *endpoint, uint64_t offset,
                   const void *data, uint32_t len)
{
    uint8_t *base;
    int rc;

    if (endpoint == NULL || endpoint->shared == NULL ||
        (data == NULL && len != 0U)) {
        return IVC_ERR_INVALID_ARG;
    }
    rc = ivc_data_validate_range(endpoint->shared, offset, len);
    if (rc != IVC_OK) {
        return rc;
    }
    base = (uint8_t *)endpoint->shared;
    if (len != 0U) {
        memcpy(base + offset, data, len);
    }
    return IVC_OK;
}

int ivc_data_read(const struct ivc_endpoint *endpoint, uint64_t offset,
                  void *data, uint32_t len)
{
    const uint8_t *base;
    int rc;

    if (endpoint == NULL || endpoint->shared == NULL ||
        (data == NULL && len != 0U)) {
        return IVC_ERR_INVALID_ARG;
    }
    rc = ivc_data_validate_range(endpoint->shared, offset, len);
    if (rc != IVC_OK) {
        return rc;
    }
    base = (const uint8_t *)endpoint->shared;
    if (len != 0U) {
        memcpy(data, base + offset, len);
    }
    return IVC_OK;
}
