/* SPDX-License-Identifier: Apache-2.0 */
#ifndef AXVISOR_IVSHMEM_IVC_DEMO_H
#define AXVISOR_IVSHMEM_IVC_DEMO_H

#include "ivc_client.h"

#ifdef __cplusplus
extern "C" {
#endif

int ivc_demo_make_image_frame(void *buffer, uint32_t capacity,
                              uint64_t image_id, uint32_t width,
                              uint32_t height, uint32_t pixel_format,
                              const void *image_data, uint32_t image_len,
                              uint32_t *payload_len_out);
int ivc_demo_parse_image_frame(const struct ivc_message *msg,
                               const struct ivc_image_frame **image_out);
int ivc_demo_make_image_desc(struct ivc_client *client, void *buffer,
                             uint32_t capacity, uint64_t image_id,
                             uint32_t width, uint32_t height,
                             uint32_t pixel_format, const void *image_data,
                             uint32_t image_len,
                             uint32_t *payload_len_out);
int ivc_demo_parse_image_desc(const struct ivc_message *msg,
                              const struct ivc_image_desc **desc_out);
int ivc_demo_read_image_desc(const struct ivc_client *client,
                             const struct ivc_image_desc *desc, void *data,
                             uint32_t capacity);
int ivc_demo_release_image_desc(struct ivc_client *client,
                                const struct ivc_image_desc *desc);

int ivc_demo_make_control_cmd(void *buffer, uint32_t capacity,
                              uint32_t command, uint32_t flags,
                              uint64_t target_id, const void *args,
                              uint32_t arg_len,
                              uint32_t *payload_len_out);
int ivc_demo_parse_control_cmd(const struct ivc_message *msg,
                               const struct ivc_control_cmd **cmd_out);

int ivc_demo_execute_control_cmd(const struct ivc_control_cmd *cmd,
                                 struct ivc_control_result *result,
                                 uint32_t *reply_type_out);
const char *ivc_demo_control_status_string(int32_t status);

int ivc_demo_make_error(void *buffer, uint32_t capacity, uint32_t code,
                        const char *detail, uint32_t *payload_len_out);
int ivc_demo_parse_error(const struct ivc_message *msg,
                         const struct ivc_error_payload **error_out);
const char *ivc_demo_error_code_string(uint32_t code);

int ivc_demo_make_heartbeat(void *buffer, uint32_t capacity, uint64_t nonce,
                            uint64_t uptime_ms, uint32_t *payload_len_out);
int ivc_demo_parse_heartbeat(const struct ivc_message *msg,
                             const struct ivc_heartbeat_payload **heartbeat_out);

#ifdef __cplusplus
}
#endif

#endif /* AXVISOR_IVSHMEM_IVC_DEMO_H */
