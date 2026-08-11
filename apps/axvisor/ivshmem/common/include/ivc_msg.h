/* SPDX-License-Identifier: Apache-2.0 */
#ifndef AXVISOR_IVSHMEM_IVC_MSG_H
#define AXVISOR_IVSHMEM_IVC_MSG_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#if defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112L
#define IVC_STATIC_ASSERT(cond, msg) _Static_assert(cond, #msg)
#else
#define IVC_STATIC_ASSERT(cond, msg) typedef char static_assertion_##msg[(cond) ? 1 : -1]
#endif

#define IVC_MSG_MAGIC 0x4956434dU    /* "IVCM" */
#define IVC_RING_MAGIC 0x49564352U   /* "IVCR" */
#define IVC_SHARED_MAGIC 0x49564353U /* "IVCS" */
#define IVC_DATA_BLOCK_MAGIC 0x49564342U /* "IVCB" */

#define IVC_MSG_VERSION 1U
#define IVC_RING_VERSION 1U
#define IVC_SHARED_VERSION 1U

#define IVC_MSG_HEADER_SIZE 48U
#define IVC_RING_HEADER_SIZE 32U
#define IVC_SHARED_HEADER_SIZE 72U
#define IVC_DATA_BLOCK_HEADER_SIZE 24U

enum ivc_msg_type {
    IVC_MSG_HELLO = 1,
    IVC_MSG_HELLO_ACK = 2,

    IVC_MSG_IMAGE_FRAME = 10,
    IVC_MSG_PROCESS_RESULT = 11,
    IVC_MSG_CONTROL_CMD = 12,
    IVC_MSG_CONTROL_DONE = 13,
    IVC_MSG_CONTROL_FAILED = 14,

    IVC_MSG_ACK = 100,
    IVC_MSG_ERROR = 101,
    IVC_MSG_HEARTBEAT = 102,
};

enum ivc_msg_flags {
    IVC_MSG_F_NONE = 0,
    IVC_MSG_F_NEEDS_REPLY = 1U << 0,
    IVC_MSG_F_IS_REPLY = 1U << 1,
};

enum ivc_data_block_flags {
    IVC_DATA_BLOCK_FREE = 1U,
    IVC_DATA_BLOCK_USED = 2U,
};

enum ivc_pixel_format {
    IVC_PIXEL_FORMAT_UNKNOWN = 0,
    IVC_PIXEL_FORMAT_GRAY8 = 1,
    IVC_PIXEL_FORMAT_RGB888 = 2,
    IVC_PIXEL_FORMAT_BGR888 = 3,
    IVC_PIXEL_FORMAT_RGBA8888 = 4,
    IVC_PIXEL_FORMAT_BGRA8888 = 5,
    IVC_PIXEL_FORMAT_YUYV = 6,
};

enum ivc_control_command {
    IVC_CMD_SET_EXPOSURE = 1,
    IVC_CMD_SET_GAIN = 2,
    IVC_CMD_CAPTURE_ONCE = 3,
    IVC_CMD_STOP_CAPTURE = 4,
};

enum ivc_control_status {
    IVC_CONTROL_OK = 0,
    IVC_CONTROL_ERR_UNKNOWN_COMMAND = -1,
    IVC_CONTROL_ERR_INVALID_ARGUMENT = -2,
};

enum ivc_error_code {
    IVC_PROTO_ERR_INVALID_MAGIC = 1,
    IVC_PROTO_ERR_UNSUPPORTED_VERSION = 2,
    IVC_PROTO_ERR_RING_FULL = 3,
    IVC_PROTO_ERR_CHECKSUM = 4,
    IVC_PROTO_ERR_TIMEOUT = 5,
    IVC_PROTO_ERR_UNKNOWN_COMMAND = 6,
};

struct ivc_msg_header {
    uint32_t magic;
    uint16_t version;
    uint16_t header_len;

    uint32_t msg_type;
    uint32_t flags;

    uint64_t seq;
    uint64_t reply_to;

    uint32_t payload_len;
    uint32_t checksum;
    uint64_t timestamp_ns;
};

struct ivc_image_frame {
    uint64_t image_id;
    uint32_t width;
    uint32_t height;
    uint32_t pixel_format;
    uint32_t data_len;
    uint8_t data[];
};

struct ivc_image_desc {
    uint64_t image_id;
    uint32_t width;
    uint32_t height;
    uint32_t pixel_format;
    uint32_t data_len;
    uint64_t data_offset;
};

struct ivc_data_block_header {
    uint32_t magic;
    uint32_t flags;
    uint64_t size;
    uint64_t next_offset;
};

struct ivc_control_cmd {
    uint32_t command;
    uint32_t flags;
    uint64_t target_id;
    uint32_t arg_len;
    uint8_t args[];
};

struct ivc_control_result {
    uint32_t command;
    int32_t status;
    uint64_t target_id;
};

struct ivc_error_payload {
    uint32_t code;
    uint32_t detail_len;
    uint8_t detail[];
};

struct ivc_heartbeat_payload {
    uint64_t nonce;
    uint64_t uptime_ms;
};

struct ivc_ring_header {
    uint32_t magic;
    uint32_t version;
    uint32_t size;
    uint32_t flags;
    uint64_t write_pos;
    uint64_t read_pos;
};

struct ivc_shared_header {
    uint32_t magic;
    uint16_t version;
    uint16_t header_len;
    uint32_t total_size;
    uint32_t flags;
    uint64_t z_to_l_offset;
    uint64_t z_to_l_size;
    uint64_t l_to_z_offset;
    uint64_t l_to_z_size;
    uint64_t data_offset;
    uint64_t data_size;
    uint64_t data_head_offset;
};

IVC_STATIC_ASSERT(sizeof(struct ivc_msg_header) == IVC_MSG_HEADER_SIZE,
                  ivc_msg_header_size);
IVC_STATIC_ASSERT(offsetof(struct ivc_msg_header, magic) == 0,
                  ivc_msg_header_magic_offset);
IVC_STATIC_ASSERT(offsetof(struct ivc_msg_header, version) == 4,
                  ivc_msg_header_version_offset);
IVC_STATIC_ASSERT(offsetof(struct ivc_msg_header, msg_type) == 8,
                  ivc_msg_header_type_offset);
IVC_STATIC_ASSERT(offsetof(struct ivc_msg_header, seq) == 16,
                  ivc_msg_header_seq_offset);
IVC_STATIC_ASSERT(offsetof(struct ivc_msg_header, reply_to) == 24,
                  ivc_msg_header_reply_to_offset);
IVC_STATIC_ASSERT(offsetof(struct ivc_msg_header, payload_len) == 32,
                  ivc_msg_header_payload_len_offset);
IVC_STATIC_ASSERT(offsetof(struct ivc_msg_header, timestamp_ns) == 40,
                  ivc_msg_header_timestamp_offset);

IVC_STATIC_ASSERT(offsetof(struct ivc_image_frame, data) == 24,
                  ivc_image_frame_data_offset);
IVC_STATIC_ASSERT(sizeof(struct ivc_image_desc) == 32,
                  ivc_image_desc_size);
IVC_STATIC_ASSERT(offsetof(struct ivc_image_desc, data_offset) == 24,
                  ivc_image_desc_data_offset_offset);
IVC_STATIC_ASSERT(sizeof(struct ivc_data_block_header) ==
                      IVC_DATA_BLOCK_HEADER_SIZE,
                  ivc_data_block_header_size);
IVC_STATIC_ASSERT(offsetof(struct ivc_data_block_header, size) == 8,
                  ivc_data_block_header_size_offset);
IVC_STATIC_ASSERT(offsetof(struct ivc_data_block_header, next_offset) == 16,
                  ivc_data_block_header_next_offset_offset);
IVC_STATIC_ASSERT(offsetof(struct ivc_control_cmd, args) == 20,
                  ivc_control_cmd_args_offset);
IVC_STATIC_ASSERT(sizeof(struct ivc_control_result) == 16,
                  ivc_control_result_size);
IVC_STATIC_ASSERT(offsetof(struct ivc_control_result, status) == 4,
                  ivc_control_result_status_offset);
IVC_STATIC_ASSERT(offsetof(struct ivc_error_payload, detail) == 8,
                  ivc_error_payload_detail_offset);
IVC_STATIC_ASSERT(sizeof(struct ivc_heartbeat_payload) == 16,
                  ivc_heartbeat_payload_size);

IVC_STATIC_ASSERT(sizeof(struct ivc_ring_header) == IVC_RING_HEADER_SIZE,
                  ivc_ring_header_size);
IVC_STATIC_ASSERT(offsetof(struct ivc_ring_header, write_pos) == 16,
                  ivc_ring_header_write_pos_offset);
IVC_STATIC_ASSERT(offsetof(struct ivc_ring_header, read_pos) == 24,
                  ivc_ring_header_read_pos_offset);

IVC_STATIC_ASSERT(sizeof(struct ivc_shared_header) == IVC_SHARED_HEADER_SIZE,
                  ivc_shared_header_size);
IVC_STATIC_ASSERT(offsetof(struct ivc_shared_header, total_size) == 8,
                  ivc_shared_header_total_size_offset);
IVC_STATIC_ASSERT(offsetof(struct ivc_shared_header, z_to_l_offset) == 16,
                  ivc_shared_header_z_to_l_offset_offset);
IVC_STATIC_ASSERT(offsetof(struct ivc_shared_header, l_to_z_offset) == 32,
                  ivc_shared_header_l_to_z_offset_offset);
IVC_STATIC_ASSERT(offsetof(struct ivc_shared_header, data_offset) == 48,
                  ivc_shared_header_data_offset_offset);
IVC_STATIC_ASSERT(offsetof(struct ivc_shared_header, data_head_offset) == 64,
                  ivc_shared_header_data_head_offset);

#ifdef __cplusplus
}
#endif

#endif /* AXVISOR_IVSHMEM_IVC_MSG_H */
