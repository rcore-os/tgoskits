/*
 * SPDX-License-Identifier: Apache-2.0
 */

#include <errno.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include <ivc.h>
#include <zephyr/kernel.h>

#include "tgos_ivc_backend.h"
#include "tgos_ivc.h"

struct ivc {
	uint8_t token;
};

static bool timeout_is_valid(int32_t timeout_ms)
{
	return timeout_ms >= 0 && timeout_ms <= IVC_MAX_TIMEOUT_MS;
}

int ivc_open(struct ivc **out)
{
	struct ivc *connection;
	int result;

	if (out == NULL) {
		return -EINVAL;
	}
	*out = NULL;

	connection = k_malloc(sizeof(*connection));
	if (connection == NULL) {
		return -ENOMEM;
	}

	result = tgos_ivc_open();
	if (result != 0) {
		k_free(connection);
		return result;
	}

	*out = connection;
	return 0;
}

int ivc_send(struct ivc *connection, const void *data, size_t len,
	     int32_t timeout_ms)
{
	if (connection == NULL || data == NULL || len == 0U) {
		return -EINVAL;
	}
	if (len > IVC_MAX_RECORD_BYTES) {
		return -EMSGSIZE;
	}
	if (!timeout_is_valid(timeout_ms)) {
		return -EINVAL;
	}

	return tgos_ivc_send(data, len, timeout_ms);
}

int ivc_recv(struct ivc *connection, void *buffer, size_t cap,
	     int32_t timeout_ms)
{
	int result;

	if (connection == NULL || buffer == NULL || cap == 0U) {
		return -EINVAL;
	}
	if (!timeout_is_valid(timeout_ms)) {
		return -EINVAL;
	}

	result = tgos_ivc_recv_record(buffer, cap, timeout_ms);
	if (result > 0) {
		if ((size_t)result > IVC_MAX_RECORD_BYTES) {
			return -EMSGSIZE;
		}
		return result;
	}

	return result;
}

int ivc_close(struct ivc *connection)
{
	int result;

	if (connection == NULL) {
		return -EINVAL;
	}

	result = tgos_ivc_close_result();
	k_free(connection);
	return result;
}
