/*
 * SPDX-License-Identifier: Apache-2.0
 */

#ifndef TGOS_IVC_H_
#define TGOS_IVC_H_

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Maximum payload carried by one migrated Message V1 record.
 *
 * Larger bulk data belongs in the bounded payload window exposed through
 * tgos_ivc_write_payload() and tgos_ivc_read_payload().
 */
#define TGOS_IVC_MAX_RECORD_BYTES 48U

/*
 * Open the single provisioned ivshmem endpoint.
 *
 * This call may succeed before the peer is ready. Session establishment is
 * completed asynchronously by subsequent send, receive, or payload calls.
 * Use tgos_ivc_wait_ready() when the application needs to establish the
 * session before performing a nonblocking payload operation.
 *
 * Must be called from task context and all public functions must be serialized
 * by the caller. Calling from interrupt context is not supported.
 *
 * @return 0 on success, otherwise a negative errno value. EBUSY means another
 *         session is open or an old failed session still requires close.
 */
int tgos_ivc_open(void);

/*
 * Wait for Message V1 session establishment to complete.
 *
 * This call is optional after tgos_ivc_open(). Send, receive, and payload
 * calls establish the session automatically, but payload calls use a
 * nonblocking readiness check. timeout_ms has the same semantics as
 * tgos_ivc_send().
 *
 * @return 0 when the session is ready, -EAGAIN if the peer is not ready or the
 *         timeout expires, -EPIPE when the session was reset, -ENODEV when no
 *         session is open, or another negative errno on failure.
 */
int tgos_ivc_wait_ready(int32_t timeout_ms);

/*
 * Close the current session and invalidate its Message V1 incarnation.
 *
 * The endpoint and its kernel mappings remain reserved for a later open.
 * This function has no effect when called from interrupt context.
 */
void tgos_ivc_close(void);

/*
 * Send one record of at most TGOS_IVC_MAX_RECORD_BYTES bytes.
 *
 * timeout_ms == 0 is nonblocking. timeout_ms > 0 bounds the wait in
 * milliseconds. timeout_ms < 0 waits indefinitely and must be used only when
 * the application explicitly permits waiting for a peer that may not start.
 *
 * @return 0 on success, -EAGAIN if the peer is not ready or the timeout
 *         expires, -EMSGSIZE for len > TGOS_IVC_MAX_RECORD_BYTES, -EPIPE when
 *         the session was reset, or another negative errno on failure.
 */
int tgos_ivc_send(const void *data, size_t len, int32_t timeout_ms);

/*
 * Receive one complete record.
 *
 * A successful call returns the positive record length. A buffer that cannot
 * hold the complete next record returns -ENOSPC without consuming that
 * record. A record larger than TGOS_IVC_MAX_RECORD_BYTES returns -EMSGSIZE
 * without consuming it. timeout_ms has the same semantics as
 * tgos_ivc_send().
 *
 * @return positive record length on success, or a negative errno value.
 */
int tgos_ivc_recv(void *data, size_t cap, int32_t timeout_ms);

/*
 * Return the local writable payload capacity after the page-aligned
 * Message V1 protocol region. Returns zero before open or after session loss.
 */
size_t tgos_ivc_payload_capacity(void);

/*
 * Copy into the local owner-only payload window.
 *
 * The peer may read the payload through tgos_ivc_read_payload(). The
 * application must use a Message V1 record to publish readiness and carry
 * the corresponding acknowledgement or credit.
 *
 * @return 0 on success, or a negative errno value.
 */
int tgos_ivc_write_payload(size_t offset, const void *data, size_t len);

/*
 * Copy from the peer-owned read-only payload window.
 *
 * @return 0 on success, or a negative errno value.
 */
int tgos_ivc_read_payload(size_t offset, void *data, size_t len);

#ifdef __cplusplus
}
#endif

#endif /* TGOS_IVC_H_ */
