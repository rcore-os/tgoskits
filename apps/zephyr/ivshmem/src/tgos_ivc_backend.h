/*
 * SPDX-License-Identifier: Apache-2.0
 */

#ifndef TGOS_IVC_BACKEND_H_
#define TGOS_IVC_BACKEND_H_

#include <stddef.h>
#include <stdint.h>

/*
 * Private bridge between the Zephyr platform adapter and the Rust static
 * library. These declarations are not part of the application-facing ABI.
 */
struct tgos_ivc_platform_view {
	uintptr_t local_output;
	uintptr_t peer_output;
	uintptr_t local_epoch;
	uintptr_t peer_epoch;
	size_t output_size;
};

int tgos_ivc_backend_lock(void);
void tgos_ivc_backend_unlock(void);

int tgos_ivc_backend_prepare(struct tgos_ivc_platform_view *view);
int tgos_ivc_backend_enable_interrupts(void);
void tgos_ivc_backend_disable_interrupts(void);
void tgos_ivc_backend_reset_event(void);
void tgos_ivc_backend_take_events(void);
int tgos_ivc_backend_wait_event(int32_t timeout_ms);
int64_t tgos_ivc_backend_now_ms(void);
int tgos_ivc_backend_epochs(uint64_t *local, uint64_t *peer);
int tgos_ivc_backend_ring_peer(void);

/*
 * Private Rust entry points used by the unified C application ABI. They share
 * the single session state and backend serialization with tgos_ivc_*.
 */
int tgos_ivc_recv_record(void *data, size_t cap, int32_t timeout_ms);
int tgos_ivc_close_result(void);

__attribute__((noreturn)) void tgos_ivc_backend_abort(void);

#endif /* TGOS_IVC_BACKEND_H_ */
