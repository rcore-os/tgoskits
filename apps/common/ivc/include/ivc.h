/* SPDX-License-Identifier: Apache-2.0 */

#ifndef IVC_H_
#define IVC_H_

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Maximum payload carried by one IVC record. */
#define IVC_MAX_RECORD_BYTES 48U

/* Largest accepted timeout_ms value. */
#define IVC_MAX_TIMEOUT_MS 60000

/* Opaque connection handle for one IVC session. */
struct ivc;

/*
 * Caller memory contract:
 *
 * out points to a writable slot for one struct ivc pointer. data addresses len
 * readable bytes and send leaves those bytes unchanged. buffer addresses cap
 * writable bytes, and recv has exclusive access to that region for the
 * duration of the call. All of these pointer slots and memory regions must
 * remain valid during the call and must not overlap a live opaque handle.
 */

/*
 * Open the application's provisioned IVC endpoint.
 *
 * A device implementation that supports only one session may return -EBUSY
 * while another session is active. Open does not wait for the peer; send and
 * receive establish the session within their own timeout.
 *
 * All calls on a live connection, including close, must be serialized by the
 * caller. Functions are not reentrant on the same connection.
 *
 * @param out Receives the opaque handle on success and is set to NULL on
 *            failure. A NULL out returns -EINVAL.
 * @return 0 on success, or a negative errno value on failure.
 */
int ivc_open(struct ivc **out);

/*
 * Send one complete record.
 *
 * Exactly one record of 1..IVC_MAX_RECORD_BYTES bytes is published on
 * success. A NULL connection, NULL data pointer, or zero length returns
 * -EINVAL; len greater than IVC_MAX_RECORD_BYTES returns -EMSGSIZE.
 *
 * timeout_ms == 0 makes one nonblocking attempt. timeout_ms in
 * 1..IVC_MAX_TIMEOUT_MS bounds the operation, including any required peer
 * handshake. Values below zero or above IVC_MAX_TIMEOUT_MS return -EINVAL.
 *
 * @return 0 on success, -EAGAIN when the session or ring capacity remains
 *         unavailable through the timeout, -EPIPE after a confirmed session
 *         reset, or another negative errno value.
 *
 * -EAGAIN means the record was not published. Other failures, including a
 * confirmed session loss, may race with the peer receiving a record; callers
 * must handle them according to the session semantics. After -EPIPE,
 * subsequent operations on this handle return -EPIPE until it is closed and
 * the connection is reopened.
 */
int ivc_send(struct ivc *connection, const void *data, size_t len,
             int32_t timeout_ms);

/*
 * Receive one complete record.
 *
 * connection and buffer must be non-NULL, and cap must be greater than zero;
 * otherwise the call returns -EINVAL. cap may exceed IVC_MAX_RECORD_BYTES. On
 * success the return value is the record length, in the range
 * 1..IVC_MAX_RECORD_BYTES.
 *
 * If the next legal record does not fit cap, the call returns -ENOSPC and that
 * record remains pending unchanged for a later call.
 *
 * The common interface only emits records of 1..IVC_MAX_RECORD_BYTES.
 * Zero-length or oversized records exposed by another low-level interface are
 * protocol-incompatible. A backend may report -EMSGSIZE, or it may close the
 * session and surface -EPIPE. The disposition of such a record is not
 * guaranteed to preserve it for a later call.
 *
 * timeout_ms == 0 makes one nonblocking attempt. timeout_ms in
 * 1..IVC_MAX_TIMEOUT_MS bounds the operation, including any required peer
 * handshake. Values below zero or above IVC_MAX_TIMEOUT_MS return -EINVAL.
 * Session handshake progress may have occurred when the timeout expires.
 *
 * @return the positive record length on success, -EAGAIN when no legal record
 *         is available through the timeout, -EPIPE after a confirmed session
 *         reset, or another negative errno value.
 *
 * After -EPIPE, subsequent operations on this handle return -EPIPE until it is
 * closed and the connection is reopened.
 */
int ivc_recv(struct ivc *connection, void *buffer, size_t cap,
             int32_t timeout_ms);

/*
 * Close a live connection.
 *
 * The caller must ensure that no other call is in progress. close consumes the
 * handle on every return path, including errors: after close returns, the
 * handle is invalid and must not be retried or used again. A NULL handle
 * returns -EINVAL.
 *
 * @return 0 on success, or a negative errno value if cleanup reported an error.
 */
int ivc_close(struct ivc *connection);

#ifdef __cplusplus
}
#endif

#endif /* IVC_H_ */
