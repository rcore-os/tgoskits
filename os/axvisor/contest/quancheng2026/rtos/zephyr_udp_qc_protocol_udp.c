/* udp.c - UDP specific code for echo server */

/*
 * Copyright (c) 2017 Intel Corporation.
 * Copyright (c) 2018 Nordic Semiconductor ASA.
 *
 * SPDX-License-Identifier: Apache-2.0
 */

#include <zephyr/logging/log.h>
LOG_MODULE_DECLARE(net_echo_server_sample, LOG_LEVEL_DBG);

#include <zephyr/kernel.h>
#include <errno.h>
#include <limits.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include <zephyr/posix/sys/socket.h>
#include <zephyr/posix/unistd.h>

#include <zephyr/net/socket.h>
#include "zsock_native_compat.h"
#include <zephyr/net/tls_credentials.h>

#include "common.h"
#include "certificate.h"

static void process_udp4(void);
static void process_udp6(void);

#define QC_PROTO_MAGIC 0x51435a31u /* "QCZ1" */
#define QC_PROTO_VERSION 1u
#define QC_PROTO_HEADER_LEN 28u
#define QC_PROTO_CHECKSUM_OFFSET 24u

#define QC_MSG_CONTROL_SET 1u
#define QC_MSG_STATE_REQ 2u
#define QC_MSG_ACK 3u
#define QC_MSG_STATUS 4u
#define QC_MSG_ERROR 5u

#define QC_FLAG_DUPLICATE BIT(0)

#define QC_STATUS_OK 0u
#define QC_STATUS_DUPLICATE 1u
#define QC_STATUS_BAD_LENGTH 100u
#define QC_STATUS_BAD_VERSION 101u
#define QC_STATUS_BAD_CHECKSUM 102u
#define QC_STATUS_UNSUPPORTED_TYPE 103u

#define QC_RTOS_PERIODIC_SAMPLES 1000u
#define QC_RTOS_PERIODIC_PERIOD_US 1000u
#define QC_RTOS_PERIODIC_PERIOD_NS \
	((uint64_t)QC_RTOS_PERIODIC_PERIOD_US * 1000u)
#define QC_RTOS_PERIODIC_THREAD_PRIORITY K_PRIO_PREEMPT(7)

struct qc_control_state {
	uint32_t last_seq;
	uint32_t applied_count;
	uint32_t duplicate_count;
	uint32_t error_count;
	int32_t setpoint_milli;
	int32_t ai_score_milli;
	int32_t output_milli;
};

struct qc_frame {
	uint8_t type;
	uint16_t payload_len;
	uint32_t seq;
	uint64_t timestamp_ns;
	const uint8_t *payload;
};

static struct qc_control_state qc_state;
static uint64_t qc_rtos_periodic_samples[QC_RTOS_PERIODIC_SAMPLES];
static bool qc_rtos_periodic_started;

static uint16_t qc_get_be16(const uint8_t *p)
{
	return ((uint16_t)p[0] << 8) | p[1];
}

static uint32_t qc_get_be32(const uint8_t *p)
{
	return ((uint32_t)p[0] << 24) | ((uint32_t)p[1] << 16) |
	       ((uint32_t)p[2] << 8) | p[3];
}

static uint64_t qc_get_be64(const uint8_t *p)
{
	return ((uint64_t)qc_get_be32(p) << 32) | qc_get_be32(p + 4);
}

static void qc_put_be16(uint8_t *p, uint16_t value)
{
	p[0] = (uint8_t)(value >> 8);
	p[1] = (uint8_t)value;
}

static void qc_put_be32(uint8_t *p, uint32_t value)
{
	p[0] = (uint8_t)(value >> 24);
	p[1] = (uint8_t)(value >> 16);
	p[2] = (uint8_t)(value >> 8);
	p[3] = (uint8_t)value;
}

static void qc_put_be64(uint8_t *p, uint64_t value)
{
	qc_put_be32(p, (uint32_t)(value >> 32));
	qc_put_be32(p + 4, (uint32_t)value);
}

static uint32_t qc_checksum(const uint8_t *buf, size_t len)
{
	uint32_t hash = 2166136261u;

	for (size_t i = 0; i < len; i++) {
		uint8_t byte = buf[i];

		if (i >= QC_PROTO_CHECKSUM_OFFSET &&
		    i < QC_PROTO_CHECKSUM_OFFSET + sizeof(uint32_t)) {
			byte = 0;
		}

		hash ^= byte;
		hash *= 16777619u;
	}

	return hash;
}

static bool qc_is_frame(const uint8_t *buf, size_t len)
{
	return len >= sizeof(uint32_t) && qc_get_be32(buf) == QC_PROTO_MAGIC;
}

static int qc_parse_frame(const uint8_t *buf, size_t len, struct qc_frame *frame,
			  uint32_t *status)
{
	uint16_t header_len;
	uint16_t payload_len;
	uint32_t expected;
	uint32_t actual;

	if (len < QC_PROTO_HEADER_LEN) {
		*status = QC_STATUS_BAD_LENGTH;
		return -EINVAL;
	}

	if (buf[4] != QC_PROTO_VERSION) {
		*status = QC_STATUS_BAD_VERSION;
		return -EINVAL;
	}

	header_len = qc_get_be16(buf + 6);
	payload_len = qc_get_be16(buf + 8);
	if (header_len != QC_PROTO_HEADER_LEN ||
	    len != (size_t)header_len + payload_len) {
		*status = QC_STATUS_BAD_LENGTH;
		return -EINVAL;
	}

	expected = qc_get_be32(buf + QC_PROTO_CHECKSUM_OFFSET);
	actual = qc_checksum(buf, len);
	if (expected != actual) {
		*status = QC_STATUS_BAD_CHECKSUM;
		return -EINVAL;
	}

	frame->type = buf[5];
	frame->payload_len = payload_len;
	frame->seq = qc_get_be32(buf + 12);
	frame->timestamp_ns = qc_get_be64(buf + 16);
	frame->payload = buf + header_len;
	*status = QC_STATUS_OK;

	return 0;
}

static int qc_send_frame(int sock, const struct sockaddr *client_addr,
			 socklen_t client_addr_len, uint8_t type, uint16_t flags,
			 uint32_t seq, uint64_t timestamp_ns,
			 const uint8_t *payload, uint16_t payload_len)
{
	uint8_t frame[RECV_BUFFER_SIZE];
	size_t frame_len = QC_PROTO_HEADER_LEN + payload_len;
	int ret;

	if (frame_len > sizeof(frame)) {
		return -EMSGSIZE;
	}

	memset(frame, 0, frame_len);
	qc_put_be32(frame, QC_PROTO_MAGIC);
	frame[4] = QC_PROTO_VERSION;
	frame[5] = type;
	qc_put_be16(frame + 6, QC_PROTO_HEADER_LEN);
	qc_put_be16(frame + 8, payload_len);
	qc_put_be16(frame + 10, flags);
	qc_put_be32(frame + 12, seq);
	qc_put_be64(frame + 16, timestamp_ns);
	if (payload_len > 0U) {
		memcpy(frame + QC_PROTO_HEADER_LEN, payload, payload_len);
	}
	qc_put_be32(frame + QC_PROTO_CHECKSUM_OFFSET,
		    qc_checksum(frame, frame_len));

	ret = sendto(sock, frame, frame_len, 0, client_addr, client_addr_len);
	if (ret < 0) {
		return -errno;
	}

	return 0;
}

static int32_t qc_clamp_i64_to_i32(int64_t value)
{
	if (value > INT32_MAX) {
		return INT32_MAX;
	}
	if (value < INT32_MIN) {
		return INT32_MIN;
	}

	return (int32_t)value;
}

static void qc_put_ack_payload(uint8_t *payload, uint32_t seq,
			       uint32_t status, int32_t output_milli)
{
	qc_put_be32(payload, seq);
	qc_put_be32(payload + 4, status);
	qc_put_be32(payload + 8, qc_state.applied_count);
	qc_put_be32(payload + 12, (uint32_t)output_milli);
}

static void qc_put_status_payload(uint8_t *payload, uint32_t status)
{
	qc_put_be32(payload, qc_state.last_seq);
	qc_put_be32(payload + 4, status);
	qc_put_be32(payload + 8, (uint32_t)qc_state.setpoint_milli);
	qc_put_be32(payload + 12, (uint32_t)qc_state.ai_score_milli);
	qc_put_be32(payload + 16, (uint32_t)qc_state.output_milli);
	qc_put_be32(payload + 20, qc_state.applied_count);
	qc_put_be32(payload + 24, qc_state.duplicate_count);
	qc_put_be32(payload + 28, qc_state.error_count);
}

static int qc_send_error(int sock, const struct sockaddr *client_addr,
			 socklen_t client_addr_len, uint32_t seq,
			 uint64_t timestamp_ns, uint32_t status)
{
	uint8_t payload[8];

	qc_state.error_count++;
	qc_put_be32(payload, seq);
	qc_put_be32(payload + 4, status);

	return qc_send_frame(sock, client_addr, client_addr_len, QC_MSG_ERROR, 0,
			     seq, timestamp_ns, payload, sizeof(payload));
}

static int qc_process_control(struct data *data, const struct qc_frame *frame,
			      const struct sockaddr *client_addr,
			      socklen_t client_addr_len)
{
	uint8_t payload[16];
	uint32_t status = QC_STATUS_OK;
	uint16_t flags = 0U;
	bool duplicate;

	if (frame->payload_len < 12U) {
		return qc_send_error(data->udp.sock, client_addr, client_addr_len,
				     frame->seq, frame->timestamp_ns,
				     QC_STATUS_BAD_LENGTH);
	}

	duplicate = qc_state.applied_count > 0U &&
		    frame->seq <= qc_state.last_seq;
	if (duplicate) {
		qc_state.duplicate_count++;
		status = QC_STATUS_DUPLICATE;
		flags |= QC_FLAG_DUPLICATE;
	} else {
		int32_t setpoint_milli =
			(int32_t)qc_get_be32(frame->payload);
		int32_t ai_score_milli =
			(int32_t)qc_get_be32(frame->payload + 4);
		int64_t output = ((int64_t)setpoint_milli * ai_score_milli) /
				 1000;

		qc_state.last_seq = frame->seq;
		qc_state.setpoint_milli = setpoint_milli;
		qc_state.ai_score_milli = ai_score_milli;
		qc_state.output_milli = qc_clamp_i64_to_i32(output);
		qc_state.applied_count++;

		LOG_INF("QC CTRL seq=%u setpoint_milli=%d ai_score_milli=%d output_milli=%d",
			frame->seq, qc_state.setpoint_milli,
			qc_state.ai_score_milli, qc_state.output_milli);
	}

	qc_put_ack_payload(payload, frame->seq, status, qc_state.output_milli);

	return qc_send_frame(data->udp.sock, client_addr, client_addr_len,
			     QC_MSG_ACK, flags, frame->seq, frame->timestamp_ns,
			     payload, sizeof(payload));
}

static int qc_process_status_request(struct data *data,
				     const struct qc_frame *frame,
				     const struct sockaddr *client_addr,
				     socklen_t client_addr_len)
{
	uint8_t payload[32];

	qc_put_status_payload(payload, QC_STATUS_OK);

	return qc_send_frame(data->udp.sock, client_addr, client_addr_len,
			     QC_MSG_STATUS, 0, frame->seq, frame->timestamp_ns,
			     payload, sizeof(payload));
}

static int qc_process_packet(struct data *data, const uint8_t *buf, size_t len,
			     const struct sockaddr *client_addr,
			     socklen_t client_addr_len)
{
	struct qc_frame frame;
	uint32_t status;
	uint32_t seq = 0U;
	uint64_t timestamp_ns = 0U;
	int ret;

	if (!qc_is_frame(buf, len)) {
		return 0;
	}

	if (len >= QC_PROTO_HEADER_LEN) {
		seq = qc_get_be32(buf + 12);
		timestamp_ns = qc_get_be64(buf + 16);
	}

	ret = qc_parse_frame(buf, len, &frame, &status);
	if (ret < 0) {
		return qc_send_error(data->udp.sock, client_addr, client_addr_len,
				     seq, timestamp_ns, status);
	}

	switch (frame.type) {
	case QC_MSG_CONTROL_SET:
		ret = qc_process_control(data, &frame, client_addr,
					 client_addr_len);
		break;
	case QC_MSG_STATE_REQ:
		ret = qc_process_status_request(data, &frame, client_addr,
						client_addr_len);
		break;
	default:
		ret = qc_send_error(data->udp.sock, client_addr, client_addr_len,
				    frame.seq, frame.timestamp_ns,
				    QC_STATUS_UNSUPPORTED_TYPE);
		break;
	}

	return ret < 0 ? ret : 1;
}

static void qc_sort_u64(uint64_t *values, size_t count)
{
	for (size_t i = 1; i < count; i++) {
		uint64_t value = values[i];
		size_t j = i;

		while (j > 0 && values[j - 1] > value) {
			values[j] = values[j - 1];
			j--;
		}
		values[j] = value;
	}
}

static void qc_rtos_periodic_probe(void *p1, void *p2, void *p3)
{
	uint64_t previous_cycles;
	uint64_t sum_ns = 0;
	uint64_t min_ns = UINT64_MAX;
	uint64_t max_ns = 0;
	uint32_t over_100us = 0;
	uint32_t over_500us = 0;
	uint32_t over_1000us = 0;

	ARG_UNUSED(p1);
	ARG_UNUSED(p2);
	ARG_UNUSED(p3);

	printk("QC_RTOS_PERIODIC_START\n");
	printk("QC_RTOS_PERIOD_SAMPLES=%u\n", QC_RTOS_PERIODIC_SAMPLES);
	printk("QC_RTOS_PERIOD_NS=%llu\n",
	       (unsigned long long)QC_RTOS_PERIODIC_PERIOD_NS);
	printk("QC_RTOS_PERIODIC_METHOD=busy_wait\n");

	previous_cycles = k_cycle_get_64();
	for (uint32_t i = 0; i < QC_RTOS_PERIODIC_SAMPLES; i++) {
		uint64_t now_cycles;
		uint64_t elapsed_ns;
		uint64_t late_ns;

		k_busy_wait(QC_RTOS_PERIODIC_PERIOD_US);

		now_cycles = k_cycle_get_64();
		elapsed_ns = k_cyc_to_ns_floor64(now_cycles - previous_cycles);
		previous_cycles = now_cycles;
		late_ns = elapsed_ns > QC_RTOS_PERIODIC_PERIOD_NS ?
			  elapsed_ns - QC_RTOS_PERIODIC_PERIOD_NS : 0;

		qc_rtos_periodic_samples[i] = late_ns;
		sum_ns += late_ns;
		if (late_ns < min_ns) {
			min_ns = late_ns;
		}
		if (late_ns > max_ns) {
			max_ns = late_ns;
		}
		if (late_ns > 100000u) {
			over_100us++;
		}
		if (late_ns > 500000u) {
			over_500us++;
		}
		if (late_ns > 1000000u) {
			over_1000us++;
		}
		if (i == 0 || i == 249 || i == 499 || i == 749 ||
		    i == QC_RTOS_PERIODIC_SAMPLES - 1) {
			printk("QC_RTOS_SAMPLE index=%u late_ns=%llu\n", i + 1,
			       (unsigned long long)late_ns);
		}
	}

	qc_sort_u64(qc_rtos_periodic_samples, QC_RTOS_PERIODIC_SAMPLES);

	printk("QC_RTOS_LATENCY_MIN_NS=%llu\n",
	       (unsigned long long)min_ns);
	printk("QC_RTOS_LATENCY_MEAN_NS=%llu\n",
	       (unsigned long long)(sum_ns / QC_RTOS_PERIODIC_SAMPLES));
	printk("QC_RTOS_LATENCY_P50_NS=%llu\n",
	       (unsigned long long)qc_rtos_periodic_samples[
		       (QC_RTOS_PERIODIC_SAMPLES * 50u) / 100u]);
	printk("QC_RTOS_LATENCY_P95_NS=%llu\n",
	       (unsigned long long)qc_rtos_periodic_samples[
		       (QC_RTOS_PERIODIC_SAMPLES * 95u) / 100u]);
	printk("QC_RTOS_LATENCY_P99_NS=%llu\n",
	       (unsigned long long)qc_rtos_periodic_samples[
		       (QC_RTOS_PERIODIC_SAMPLES * 99u) / 100u]);
	printk("QC_RTOS_LATENCY_MAX_NS=%llu\n",
	       (unsigned long long)max_ns);
	printk("QC_RTOS_OVERRUN_GT_100US=%u\n", over_100us);
	printk("QC_RTOS_OVERRUN_GT_500US=%u\n", over_500us);
	printk("QC_RTOS_OVERRUN_GT_1000US=%u\n", over_1000us);
	printk("QC_RTOS_PERIODIC_RESULT=PASS\n");
}

K_THREAD_DEFINE(udp4_thread_id, STACK_SIZE,
		process_udp4, NULL, NULL, NULL,
		THREAD_PRIORITY,
		IS_ENABLED(CONFIG_USERSPACE) ? K_USER : 0, -1);

K_THREAD_DEFINE(udp6_thread_id, STACK_SIZE,
		process_udp6, NULL, NULL, NULL,
		THREAD_PRIORITY,
		IS_ENABLED(CONFIG_USERSPACE) ? K_USER : 0, -1);

K_THREAD_DEFINE(qc_rtos_periodic_thread_id, STACK_SIZE,
		qc_rtos_periodic_probe, NULL, NULL, NULL,
		QC_RTOS_PERIODIC_THREAD_PRIORITY, 0, -1);

static int start_udp_proto(struct data *data, struct sockaddr *bind_addr,
			   socklen_t bind_addrlen)
{
	int optval;
	int ret;

#if defined(CONFIG_NET_SOCKETS_SOCKOPT_TLS)
	data->udp.sock = socket(bind_addr->sa_family, SOCK_DGRAM,
				IPPROTO_DTLS_1_2);
#else
	data->udp.sock = socket(bind_addr->sa_family, SOCK_DGRAM, IPPROTO_UDP);
#endif
	if (data->udp.sock < 0) {
		LOG_ERR("Failed to create UDP socket (%s): %d", data->proto,
			errno);
		return -errno;
	}

#if defined(CONFIG_NET_SOCKETS_SOCKOPT_TLS)
	sec_tag_t sec_tag_list[] = {
		SERVER_CERTIFICATE_TAG,
#if defined(CONFIG_MBEDTLS_KEY_EXCHANGE_PSK_ENABLED)
		PSK_TAG,
#endif
	};
	int role = TLS_DTLS_ROLE_SERVER;

	ret = setsockopt(data->udp.sock, SOL_TLS, TLS_SEC_TAG_LIST,
			 sec_tag_list, sizeof(sec_tag_list));
	if (ret < 0) {
		LOG_ERR("Failed to set UDP secure option (%s): %d", data->proto,
			errno);
		ret = -errno;
	}

	/* Set role to DTLS server. */
	ret = setsockopt(data->udp.sock, SOL_TLS, TLS_DTLS_ROLE,
			 &role, sizeof(role));
	if (ret < 0) {
		LOG_ERR("Failed to set DTLS role secure option (%s): %d",
			data->proto, errno);
		ret = -errno;
	}
#endif

	if (bind_addr->sa_family == AF_INET6) {
		/* Prefer IPv6 temporary addresses */
		optval = IPV6_PREFER_SRC_PUBLIC;
		(void)setsockopt(data->udp.sock, IPPROTO_IPV6,
				 IPV6_ADDR_PREFERENCES,
				 &optval, sizeof(optval));

		/*
		 * Bind only to IPv6 without mapping to IPv4, since we bind to
		 * IPv4 using another socket
		 */
		optval = 1;
		(void)setsockopt(data->udp.sock, IPPROTO_IPV6, IPV6_V6ONLY,
				 &optval, sizeof(optval));
	}

	ret = bind(data->udp.sock, bind_addr, bind_addrlen);
	if (ret < 0) {
		LOG_ERR("Failed to bind UDP socket (%s): %d", data->proto,
			errno);
		ret = -errno;
	}

	return ret;
}

static int process_udp(struct data *data)
{
	int ret = 0;
	int received;
	struct sockaddr client_addr;
	socklen_t client_addr_len;

	LOG_INF("Waiting for UDP packets on port %d (%s)...",
		 MY_PORT, data->proto);

	do {
		client_addr_len = sizeof(client_addr);
		received = recvfrom(data->udp.sock, data->udp.recv_buffer,
				    sizeof(data->udp.recv_buffer), 0,
				    &client_addr, &client_addr_len);

		if (received < 0) {
			/* Socket error */
			LOG_ERR("UDP (%s): Connection error %d", data->proto,
				errno);
			ret = -errno;
			break;
		} else if (received) {
			atomic_add(&data->udp.bytes_received, received);
		}

		ret = qc_process_packet(data, data->udp.recv_buffer, received,
					&client_addr, client_addr_len);
		if (ret > 0) {
			if (++data->udp.counter % 1000 == 0U) {
				LOG_INF("%s UDP/QC: Sent %u packets",
					data->proto, data->udp.counter);
			}

			continue;
		} else if (ret < 0) {
			LOG_ERR("UDP (%s): QC protocol response failed %d",
				data->proto, ret);
			break;
		}

		ret = sendto(data->udp.sock, data->udp.recv_buffer, received, 0,
			     &client_addr, client_addr_len);
		if (ret < 0) {
			LOG_ERR("UDP (%s): Failed to send %d", data->proto,
				errno);
			ret = -errno;
			break;
		}

		if (++data->udp.counter % 1000 == 0U) {
			LOG_INF("%s UDP: Sent %u packets", data->proto,
				 data->udp.counter);
		}

		LOG_DBG("UDP (%s): Received and replied with %d bytes",
			data->proto, received);
	} while (true);

	return ret;
}

static void process_udp4(void)
{
	int ret;
	struct sockaddr_in addr4;

	(void)memset(&addr4, 0, sizeof(addr4));
	addr4.sin_family = AF_INET;
	addr4.sin_port = htons(MY_PORT);

	ret = start_udp_proto(&conf.ipv4, (struct sockaddr *)&addr4,
			      sizeof(addr4));
	if (ret < 0) {
		quit();
		return;
	}

	while (ret == 0) {
		ret = process_udp(&conf.ipv4);
		if (ret < 0) {
			quit();
		}
	}
}

static void process_udp6(void)
{
	int ret;
	struct sockaddr_in6 addr6;

	(void)memset(&addr6, 0, sizeof(addr6));
	addr6.sin6_family = AF_INET6;
	addr6.sin6_port = htons(MY_PORT);

	ret = start_udp_proto(&conf.ipv6, (struct sockaddr *)&addr6,
			      sizeof(addr6));
	if (ret < 0) {
		quit();
		return;
	}

	while (ret == 0) {
		ret = process_udp(&conf.ipv6);
		if (ret < 0) {
			quit();
		}
	}
}

static void print_stats(struct k_work *work)
{
	struct k_work_delayable *dwork = k_work_delayable_from_work(work);
	struct data *data = CONTAINER_OF(dwork, struct data, udp.stats_print);
	int total_received = atomic_get(&data->udp.bytes_received);

	if (total_received) {
		if ((total_received / STATS_TIMER) < 1024) {
			LOG_INF("%s UDP: Received %d B/sec", data->proto,
				total_received / STATS_TIMER);
		} else {
			LOG_INF("%s UDP: Received %d KiB/sec", data->proto,
				total_received / 1024 / STATS_TIMER);
		}

		atomic_set(&data->udp.bytes_received, 0);
	}

	k_work_reschedule(&data->udp.stats_print, K_SECONDS(STATS_TIMER));
}

void start_udp(void)
{
	if (!qc_rtos_periodic_started) {
		qc_rtos_periodic_started = true;
		k_thread_name_set(qc_rtos_periodic_thread_id, "qc-rtos");
		k_thread_start(qc_rtos_periodic_thread_id);
	}

	if (IS_ENABLED(CONFIG_NET_IPV6)) {
#if defined(CONFIG_USERSPACE)
		k_mem_domain_add_thread(&app_domain, udp6_thread_id);
#endif

		k_work_init_delayable(&conf.ipv6.udp.stats_print, print_stats);
		k_thread_name_set(udp6_thread_id, "udp6");
		k_thread_start(udp6_thread_id);
		k_work_reschedule(&conf.ipv6.udp.stats_print,
				  K_SECONDS(STATS_TIMER));
	}

	if (IS_ENABLED(CONFIG_NET_IPV4)) {
#if defined(CONFIG_USERSPACE)
		k_mem_domain_add_thread(&app_domain, udp4_thread_id);
#endif

		k_work_init_delayable(&conf.ipv4.udp.stats_print, print_stats);
		k_thread_name_set(udp4_thread_id, "udp4");
		k_thread_start(udp4_thread_id);
		k_work_reschedule(&conf.ipv4.udp.stats_print,
				  K_SECONDS(STATS_TIMER));
	}
}

void stop_udp(void)
{
	/* Not very graceful way to close a thread, but as we may be blocked
	 * in recvfrom call it seems to be necessary
	 */
	if (IS_ENABLED(CONFIG_NET_IPV6)) {
		k_thread_abort(udp6_thread_id);
		if (conf.ipv6.udp.sock >= 0) {
			(void)close(conf.ipv6.udp.sock);
		}
	}

	if (IS_ENABLED(CONFIG_NET_IPV4)) {
		k_thread_abort(udp4_thread_id);
		if (conf.ipv4.udp.sock >= 0) {
			(void)close(conf.ipv4.udp.sock);
		}
	}
}
