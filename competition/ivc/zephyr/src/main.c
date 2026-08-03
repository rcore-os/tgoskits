/* SPDX-License-Identifier: Apache-2.0 */

#include "endpoint.h"
#include "protocol.h"

#include <errno.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

#include <zephyr/kernel.h>
#include <zephyr/net/net_if.h>
#include <zephyr/net/socket.h>
#include <zephyr/sys/printk.h>
#if CONFIG_IVC_EXIT_AFTER_EXPECTED_COMMANDS
#include <zephyr/sys/poweroff.h>
#endif

#define IVC_LOCAL_IPV4 "10.0.0.2"
#define IVC_LOCAL_UDP_PORT 5500U
#define IVC_SOCKET_POLL_MS 100
#define IVC_ETHERNET_ADDRESS_LENGTH 6U
#define IVC_ERROR_EVIDENCE_CAPACITY 8U
#define IVC_ERROR_EVIDENCE_BODY_CAPACITY 96U
#define IVC_ERROR_EVIDENCE_REPLAY_COPIES 2U
#define IVC_READY_RECORD_COPIES 2U
#define IVC_RESULT_RECORD_COPIES 2U
#define IVC_RESULT_RECORD_PAUSE_MS 10

static const uint8_t expected_mac[IVC_ETHERNET_ADDRESS_LENGTH] = {
	0x52, 0x54, 0x00, 0x00, 0x00, 0x02,
};

static uint8_t receive_frame[IVC_MAX_FRAME_LENGTH];
static uint8_t transmit_frame[IVC_MAX_FRAME_LENGTH];

struct ivc_error_evidence {
	uint32_t sequence;
	enum ivc_error_code error_code;
	const char *reason;
};

struct ivc_server {
	struct ivc_receive_window receive_window;
	struct ivc_endpoint endpoint;
	struct ivc_thermal_plant plant;
	struct ivc_ack_loss_policy ack_loss;
	uint32_t applied_commands;
	uint64_t protocol_errors;
	uint64_t status_sent;
	uint64_t acknowledgements_sent;
	uint64_t errors_sent;
	uint64_t safe_fallbacks;
	uint64_t recoveries;
	uint64_t stale_status_sent;
	uint64_t stale_acknowledgements_sent;
	struct ivc_error_evidence error_evidence[IVC_ERROR_EVIDENCE_CAPACITY];
	uint32_t error_evidence_count;
	bool error_evidence_replayed;
	bool ready_replayed;
	bool result_reported;
};

static uint64_t monotonic_us(void)
{
	return (uint64_t)k_uptime_get() * UINT64_C(1000);
}

static void report_error_evidence(const struct ivc_error_evidence *evidence)
{
	char body[IVC_ERROR_EVIDENCE_BODY_CAPACITY];
	int length;

	length = snprintk(body, sizeof(body), "seq=%u code=%u reason=%s", evidence->sequence,
			  (unsigned int)evidence->error_code, evidence->reason);
	if (length < 0 || (size_t)length >= sizeof(body)) {
		printk("IVC-RTOS-FATAL stage=error-evidence-format\n");
		return;
	}
	printk("IVC-ERROR-Z %s crc=%08x\n", body,
	       (unsigned int)ivc_crc32_bytes((const uint8_t *)body, (size_t)length));
}

static bool validate_fault_configuration(void)
{
	const uint32_t drop_every = (uint32_t)CONFIG_IVC_DROP_ACK_EVERY;
	const uint32_t expected_commands = (uint32_t)CONFIG_IVC_EXPECTED_COMMANDS;
	const uint32_t expected_errors = (uint32_t)CONFIG_IVC_EXPECTED_PROTOCOL_ERRORS;
	const uint32_t expected_session_resets = (uint32_t)CONFIG_IVC_EXPECTED_SESSION_RESETS;
	const uint32_t expected_session_rejections =
		(uint32_t)CONFIG_IVC_EXPECTED_SESSION_REJECTIONS;
	const uint32_t expected_safe_fallbacks =
		(uint32_t)CONFIG_IVC_EXPECTED_SAFE_FALLBACKS;
	const bool restart_profile = expected_session_resets != 0U ||
				     expected_session_rejections != 0U ||
				     expected_safe_fallbacks != 0U;

	if ((drop_every != 0U && expected_commands == 0U) || drop_every > expected_commands ||
	    (expected_errors != 0U && expected_commands == 0U) ||
	    (expected_errors != 0U && drop_every != 0U) ||
	    (restart_profile && expected_commands == 0U) ||
	    (restart_profile && drop_every != 0U) ||
	    (restart_profile && (expected_session_resets == 0U ||
				 expected_session_rejections == 0U ||
				 expected_safe_fallbacks == 0U)) ||
	    (restart_profile && expected_errors != expected_session_rejections) ||
	    expected_errors > IVC_ERROR_EVIDENCE_CAPACITY) {
		printk("IVC-RTOS-FATAL stage=fault-config drop_ack_every=%u "
		       "expected_commands=%u expected_protocol_errors=%u session_resets=%u "
		       "session_rejections=%u safe_fallbacks=%u evidence_capacity=%u\n",
		       drop_every, expected_commands, expected_errors,
		       expected_session_resets, expected_session_rejections,
		       expected_safe_fallbacks,
		       (unsigned int)IVC_ERROR_EVIDENCE_CAPACITY);
		return false;
	}
	return true;
}

static bool validate_network_identity(void)
{
	struct net_if *interface = net_if_get_default();
	const struct net_linkaddr *address;

	if (interface == NULL) {
		printk("IVC-RTOS-NET-ERROR reason=no-default-interface\n");
		return false;
	}
	address = net_if_get_link_addr(interface);
	if (address == NULL || address->len != sizeof(expected_mac)) {
		printk("IVC-RTOS-NET-ERROR reason=invalid-mac-length\n");
		return false;
	}
	printk("IVC-RTOS-NET mac=%02x:%02x:%02x:%02x:%02x:%02x expected="
	       "52:54:00:00:00:02\n",
	       (unsigned int)address->addr[0], (unsigned int)address->addr[1],
	       (unsigned int)address->addr[2], (unsigned int)address->addr[3],
	       (unsigned int)address->addr[4], (unsigned int)address->addr[5]);
	if (memcmp(address->addr, expected_mac, sizeof(expected_mac)) != 0) {
		printk("IVC-RTOS-NET-ERROR reason=unexpected-mac\n");
		return false;
	}
	return true;
}

static int open_endpoint_socket(void)
{
	struct sockaddr_in local = {
		.sin_family = AF_INET,
		.sin_port = htons(IVC_LOCAL_UDP_PORT),
	};
	int socket_fd;

	if (zsock_inet_pton(AF_INET, IVC_LOCAL_IPV4, &local.sin_addr) != 1) {
		printk("IVC-RTOS-FATAL stage=parse-bind-address errno=%d\n", errno);
		return -1;
	}
	socket_fd = zsock_socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
	if (socket_fd < 0) {
		printk("IVC-RTOS-FATAL stage=socket errno=%d\n", errno);
		return -1;
	}
	if (zsock_bind(socket_fd, (const struct sockaddr *)&local, sizeof(local)) < 0) {
		printk("IVC-RTOS-FATAL stage=bind ip=%s port=%u errno=%d\n", IVC_LOCAL_IPV4,
		       IVC_LOCAL_UDP_PORT, errno);
		(void)zsock_close(socket_fd);
		return -1;
	}
	return socket_fd;
}

static bool send_payload(int socket_fd, const struct sockaddr *peer, socklen_t peer_length,
			 const struct ivc_header *request, enum ivc_message_type message_type,
			 enum ivc_error_code error_code, const uint8_t *payload,
			 uint16_t payload_length)
{
	const struct ivc_header response = {
		.message_type = message_type,
		.flags = 0U,
		.session_id = request->session_id,
		.sequence = request->sequence,
		.timestamp_us = monotonic_us(),
		.payload_length = payload_length,
		.error_code = error_code,
	};
	size_t frame_length;
	ssize_t sent;

	if (!ivc_encode_frame(&response, payload, transmit_frame, sizeof(transmit_frame),
			      &frame_length)) {
		printk("IVC-RTOS-FATAL stage=encode-response seq=%u type=%u\n", request->sequence,
		       (unsigned int)message_type);
		return false;
	}
	sent = zsock_sendto(socket_fd, transmit_frame, frame_length, 0, peer, peer_length);
	if (sent != (ssize_t)frame_length) {
		printk("IVC-RTOS-TX-ERROR seq=%u type=%u sent=%d expected=%u errno=%d\n",
		       request->sequence, (unsigned int)message_type, (int)sent,
		       (unsigned int)frame_length, errno);
		return false;
	}
	return true;
}

static bool send_error(int socket_fd, const struct sockaddr *peer, socklen_t peer_length,
		       const struct ivc_header *request, enum ivc_error_code error_code)
{
	const struct ivc_error_payload error = {
		.offending_message_type = request->message_type,
		.offending_sequence = request->sequence,
	};
	uint8_t payload[IVC_ERROR_PAYLOAD_LENGTH];

	return ivc_encode_error_payload(&error, payload) &&
	       send_payload(socket_fd, peer, peer_length, request, IVC_MESSAGE_ERROR, error_code,
			    payload, sizeof(payload));
}

static bool send_status(int socket_fd, const struct sockaddr *peer, socklen_t peer_length,
			const struct ivc_header *request, const struct ivc_endpoint *endpoint,
			const struct ivc_thermal_plant *plant)
{
	const struct ivc_status_report status =
		ivc_endpoint_status(endpoint, ivc_thermal_plant_temperature(plant));
	uint8_t payload[IVC_STATUS_PAYLOAD_LENGTH];

	return ivc_encode_status(&status, payload) &&
	       send_payload(socket_fd, peer, peer_length, request, IVC_MESSAGE_STATUS,
			    IVC_ERROR_NONE, payload, sizeof(payload));
}

static bool send_ack(int socket_fd, const struct sockaddr *peer, socklen_t peer_length,
		     const struct ivc_header *request, const struct ivc_receive_window *window)
{
	const struct ivc_ack_payload ack = ivc_receive_window_ack(window, request->sequence);
	uint8_t payload[IVC_ACK_PAYLOAD_LENGTH];

	return ivc_encode_ack(&ack, payload) &&
	       send_payload(socket_fd, peer, peer_length, request, IVC_MESSAGE_ACK, IVC_ERROR_NONE,
			    payload, sizeof(payload));
}

static void report_ready(void)
{
	printk("IVC-RTOS-READY bind=%s:%u mac=52:54:00:00:00:02 window_bits=%u "
	       "ack_loss_drop_every=%u expected_commands=%u expected_protocol_errors=%u "
	       "exit_after_expected=%u\n",
	       IVC_LOCAL_IPV4, IVC_LOCAL_UDP_PORT, IVC_RECEIVE_WINDOW_BITS,
	       (uint32_t)CONFIG_IVC_DROP_ACK_EVERY, (uint32_t)CONFIG_IVC_EXPECTED_COMMANDS,
	       (uint32_t)CONFIG_IVC_EXPECTED_PROTOCOL_ERRORS,
	       (uint32_t)IS_ENABLED(CONFIG_IVC_EXIT_AFTER_EXPECTED_COMMANDS));
}

static void report_restart_ready(void)
{
	if (CONFIG_IVC_EXPECTED_SESSION_RESETS == 0) {
		return;
	}
	printk("IVC-RTOS-RESTART-READY commands=%u errors=%u resets=%u rejections=%u "
	       "safe=%u drop=%u exit=%u\n",
	       (uint32_t)CONFIG_IVC_EXPECTED_COMMANDS,
	       (uint32_t)CONFIG_IVC_EXPECTED_PROTOCOL_ERRORS,
	       (uint32_t)CONFIG_IVC_EXPECTED_SESSION_RESETS,
	       (uint32_t)CONFIG_IVC_EXPECTED_SESSION_REJECTIONS,
	       (uint32_t)CONFIG_IVC_EXPECTED_SAFE_FALLBACKS,
	       (uint32_t)CONFIG_IVC_DROP_ACK_EVERY,
	       (uint32_t)IS_ENABLED(CONFIG_IVC_EXIT_AFTER_EXPECTED_COMMANDS));
}

static void replay_ready_if_needed(struct ivc_server *server)
{
	uint32_t copy;

	if (server->ready_replayed) {
		return;
	}
	server->ready_replayed = true;
	for (copy = 0U; copy < IVC_READY_RECORD_COPIES; ++copy) {
		report_ready();
		report_restart_ready();
		k_sleep(K_MSEC(IVC_RESULT_RECORD_PAUSE_MS));
	}
}

static void report_compact_result(const struct ivc_server *server, const char *profile)
{
	uint32_t copy;

	for (copy = 0U; copy < IVC_RESULT_RECORD_COPIES; ++copy) {
		printk("IVC-RTOS-OUTCOME profile=%s accepted=%llu applied=%u duplicates=%llu "
		       "acks_dropped=%llu\n",
		       profile, server->receive_window.metrics.accepted,
		       server->applied_commands, server->receive_window.metrics.duplicates,
		       server->ack_loss.acknowledgements_dropped);
		k_sleep(K_MSEC(IVC_RESULT_RECORD_PAUSE_MS));
		printk("IVC-RTOS-MESSAGES status_sent=%llu acks_sent=%llu errors_sent=%llu "
		       "protocol_errors=%llu\n",
		       server->status_sent, server->acknowledgements_sent,
		       server->errors_sent, server->protocol_errors);
		k_sleep(K_MSEC(IVC_RESULT_RECORD_PAUSE_MS));
		if (strcmp(profile, "restart") == 0) {
			printk("IVC-RTOS-RESTART session_resets=%llu session_rejections=%llu "
			       "safe_fallbacks=%llu recoveries=%llu stale_status_sent=%llu "
			       "stale_acks_sent=%llu\n",
			       server->receive_window.metrics.session_resets,
			       server->receive_window.metrics.session_rejections,
			       server->safe_fallbacks, server->recoveries,
			       server->stale_status_sent,
			       server->stale_acknowledgements_sent);
			k_sleep(K_MSEC(IVC_RESULT_RECORD_PAUSE_MS));
		}
	}
}

static void report_result_if_complete(struct ivc_server *server)
{
	const uint32_t expected_commands = (uint32_t)CONFIG_IVC_EXPECTED_COMMANDS;
#if CONFIG_IVC_EXPECTED_SESSION_RESETS > 0
	const uint64_t expected_errors = (uint64_t)CONFIG_IVC_EXPECTED_PROTOCOL_ERRORS;
	const uint64_t expected_session_resets = (uint64_t)CONFIG_IVC_EXPECTED_SESSION_RESETS;
	const uint64_t expected_session_rejections =
		(uint64_t)CONFIG_IVC_EXPECTED_SESSION_REJECTIONS;
	const uint64_t expected_safe_fallbacks =
		(uint64_t)CONFIG_IVC_EXPECTED_SAFE_FALLBACKS;
	const char *profile = "restart";
#elif CONFIG_IVC_EXPECTED_PROTOCOL_ERRORS > 0
	const uint64_t expected_errors = (uint64_t)CONFIG_IVC_EXPECTED_PROTOCOL_ERRORS;
	const char *profile = "error";
#elif CONFIG_IVC_DROP_ACK_EVERY > 0
	const uint64_t expected_drops =
		(uint64_t)(expected_commands / (uint32_t)CONFIG_IVC_DROP_ACK_EVERY);
	const char *profile = "ack-loss";
#else
	const char *profile = "normal";
#endif

	if (server->result_reported || expected_commands == 0U ||
	    server->receive_window.metrics.accepted != expected_commands) {
		return;
	}
#if CONFIG_IVC_EXPECTED_SESSION_RESETS > 0
	if (server->protocol_errors != expected_errors || server->errors_sent != expected_errors ||
	    !server->error_evidence_replayed ||
	    server->receive_window.metrics.session_resets != expected_session_resets ||
	    server->receive_window.metrics.session_rejections != expected_session_rejections ||
	    server->safe_fallbacks != expected_safe_fallbacks ||
	    server->recoveries != expected_safe_fallbacks || server->stale_status_sent != 1U ||
	    server->stale_acknowledgements_sent != 1U) {
		return;
	}
#elif CONFIG_IVC_EXPECTED_PROTOCOL_ERRORS > 0
	if (server->protocol_errors != expected_errors || server->errors_sent != expected_errors ||
	    !server->error_evidence_replayed) {
		return;
	}
#elif CONFIG_IVC_DROP_ACK_EVERY > 0
	if (server->receive_window.metrics.duplicates != expected_drops ||
	    server->ack_loss.acknowledgements_dropped != expected_drops) {
		return;
	}
#endif
	report_compact_result(server, profile);
	printk("IVC-RTOS-RESULT profile=%s accepted=%llu applied=%u duplicates=%llu "
	       "acks_dropped=%llu status_sent=%llu acks_sent=%llu errors_sent=%llu "
	       "protocol_errors=%llu\n",
	       profile, server->receive_window.metrics.accepted, server->applied_commands,
	       server->receive_window.metrics.duplicates,
	       server->ack_loss.acknowledgements_dropped, server->status_sent,
	       server->acknowledgements_sent, server->errors_sent, server->protocol_errors);
	server->result_reported = true;
#if CONFIG_IVC_EXIT_AFTER_EXPECTED_COMMANDS
	for (uint32_t copy = 0U; copy < IVC_RESULT_RECORD_COPIES; ++copy) {
		printk("IVC-RTOS-POWEROFF accepted=%llu\n",
		       server->receive_window.metrics.accepted);
		k_sleep(K_MSEC(IVC_RESULT_RECORD_PAUSE_MS));
	}
	k_sleep(K_MSEC(100));
	sys_poweroff();
#endif
}

static enum ivc_error_code apply_error_code(enum ivc_apply_result result)
{
	if (result == IVC_APPLY_INVALID_PAYLOAD) {
		return IVC_ERROR_INVALID_CONTROL;
	}
	return IVC_ERROR_STALE_CONTROL;
}

static void reject_datagram(struct ivc_server *server, int socket_fd,
			    const struct sockaddr *peer, socklen_t peer_length,
			    const struct ivc_header *request, enum ivc_error_code error_code,
			    const char *reason)
{
	const struct ivc_error_evidence evidence = {
		.sequence = request->sequence,
		.error_code = error_code,
		.reason = reason,
	};

	++server->protocol_errors;
	if (server->error_evidence_count < IVC_ERROR_EVIDENCE_CAPACITY) {
		server->error_evidence[server->error_evidence_count++] = evidence;
	}
	report_error_evidence(&evidence);
	if (send_error(socket_fd, peer, peer_length, request, error_code)) {
		++server->errors_sent;
	}
}

static void replay_error_evidence_if_complete(struct ivc_server *server)
{
	const uint32_t expected_commands = (uint32_t)CONFIG_IVC_EXPECTED_COMMANDS;
	const uint32_t expected_errors = (uint32_t)CONFIG_IVC_EXPECTED_PROTOCOL_ERRORS;
	uint32_t copy;
	uint32_t index;

	if (server->error_evidence_replayed || expected_errors == 0U ||
	    server->receive_window.metrics.accepted != expected_commands ||
	    server->protocol_errors != expected_errors || server->errors_sent != expected_errors ||
	    server->error_evidence_count != expected_errors) {
		return;
	}
	for (copy = 0U; copy < IVC_ERROR_EVIDENCE_REPLAY_COPIES; ++copy) {
		for (index = 0U; index < server->error_evidence_count; ++index) {
			const struct ivc_error_evidence *evidence = &server->error_evidence[index];

			report_error_evidence(evidence);
		}
	}
	server->error_evidence_replayed = true;
}

static void process_control(struct ivc_server *server, int socket_fd,
			    const struct sockaddr *peer, socklen_t peer_length,
			    const struct ivc_frame_view *frame,
			    const struct ivc_control_command *command)
{
	enum ivc_delivery delivery;
	enum ivc_apply_result apply_result;
	bool drop_ack;
	bool had_previous_session;
	bool was_safe_fallback;
	uint32_t previous_sequence;
	uint32_t previous_session;
	uint64_t received_us;

	had_previous_session = server->receive_window.has_session;
	previous_session = server->receive_window.session_id;
	previous_sequence = server->endpoint.last_sequence;
	was_safe_fallback = server->endpoint.fault == IVC_ERROR_CONTROLLER_TIMEOUT;
	delivery = ivc_receive_window_observe(&server->receive_window, frame->header.session_id,
					      frame->header.sequence);
	if (delivery == IVC_DELIVERY_NEW_OUT_OF_ORDER ||
	    delivery == IVC_DELIVERY_OUTSIDE_WINDOW) {
		reject_datagram(server, socket_fd, peer, peer_length, &frame->header,
			       IVC_ERROR_SEQUENCE_OUTSIDE_WINDOW, "sequence-outside-window");
		return;
	}
	if (delivery == IVC_DELIVERY_SESSION_REJECTED) {
		reject_datagram(server, socket_fd, peer, peer_length, &frame->header,
			       IVC_ERROR_SEQUENCE_OUTSIDE_WINDOW, "retired-or-invalid-session");
		return;
	}
	if (delivery == IVC_DELIVERY_INVALID_IDENTIFIER) {
		reject_datagram(server, socket_fd, peer, peer_length, &frame->header,
			       IVC_ERROR_SEQUENCE_OUTSIDE_WINDOW, "zero-session-or-sequence");
		return;
	}
	if (delivery == IVC_DELIVERY_SEQUENCE_EXHAUSTED) {
		reject_datagram(server, socket_fd, peer, peer_length, &frame->header,
			       IVC_ERROR_INTERNAL, "sequence-exhausted");
		return;
	}

	if (ivc_delivery_applies_control(delivery)) {
		if (delivery == IVC_DELIVERY_NEW_SESSION) {
			if (had_previous_session && CONFIG_IVC_EXPECTED_SESSION_RESETS > 0) {
				struct ivc_header stale_request = frame->header;

				stale_request.session_id = previous_session;
				stale_request.sequence = previous_sequence;
				if (send_status(socket_fd, peer, peer_length, &stale_request,
						&server->endpoint, &server->plant)) {
					++server->status_sent;
					++server->stale_status_sent;
				}
				if (send_ack(socket_fd, peer, peer_length, &stale_request,
					     &server->receive_window)) {
					++server->acknowledgements_sent;
					++server->stale_acknowledgements_sent;
				}
				printk("IVC-RTOS-STALE-REPLAY old_session=%u old_sequence=%u "
				       "new_session=%u stale_status_sent=%llu stale_acks_sent=%llu\n",
				       previous_session, previous_sequence, frame->header.session_id,
				       server->stale_status_sent,
				       server->stale_acknowledgements_sent);
			}
			ivc_endpoint_begin_session(&server->endpoint);
		}
		received_us = monotonic_us();
		/* Guest monotonic clocks do not share an epoch. As in the Rust endpoint,
		 * local receive time drives safety age and timeout checks.
		 */
		apply_result = ivc_endpoint_apply(&server->endpoint, frame->header.sequence, command,
					  received_us, received_us);
		if (apply_result != IVC_APPLY_APPLIED &&
		    apply_result != IVC_APPLY_ENTERED_SAFE_STATE) {
			reject_datagram(server, socket_fd, peer, peer_length, &frame->header,
				       apply_error_code(apply_result),
				       ivc_apply_result_name(apply_result));
			return;
		}
		if (was_safe_fallback) {
			++server->recoveries;
			printk("IVC-RTOS-RECOVERY session=%u seq=%u from=controller-timeout "
			       "mode=%s actuator_permille=%u recoveries=%llu\n",
			       frame->header.session_id, frame->header.sequence,
			       ivc_control_mode_name(command->mode),
			       (unsigned int)server->endpoint.actuator_permille,
			       server->recoveries);
		}
		ivc_thermal_plant_step(&server->plant, server->endpoint.actuator_permille,
				       server->applied_commands);
		++server->applied_commands;
		if (server->applied_commands == 1U ||
		    (server->applied_commands % 100U) == 0U ||
		    (CONFIG_IVC_EXPECTED_COMMANDS != 0 &&
		     server->applied_commands == (uint32_t)CONFIG_IVC_EXPECTED_COMMANDS)) {
			printk("IVC-RTOS-PROGRESS accepted=%u seq=%u mode=%s "
			       "actuator_permille=%u measured_milli_c=%d duplicates=%llu "
			       "protocol_errors=%llu\n",
			       server->applied_commands, frame->header.sequence,
			       ivc_control_mode_name(command->mode),
			       (unsigned int)server->endpoint.actuator_permille,
			       ivc_thermal_plant_temperature(&server->plant),
			       server->receive_window.metrics.duplicates, server->protocol_errors);
		}
	} else {
		printk("IVC-RTOS-DUPLICATE seq=%u next_expected=%u duplicates=%llu\n",
		       frame->header.sequence, server->receive_window.next_sequence,
		       server->receive_window.metrics.duplicates);
	}

	replay_error_evidence_if_complete(server);
	/* The controller waits for both messages and the Rust endpoint sends STATUS first. */
	if (send_status(socket_fd, peer, peer_length, &frame->header, &server->endpoint,
			&server->plant)) {
		++server->status_sent;
	}
	drop_ack = ivc_ack_loss_policy_should_drop(&server->ack_loss, delivery,
					    frame->header.sequence);
	if (drop_ack) {
		printk("IVC-RTOS-INJECT drop_ack_seq=%u\n", frame->header.sequence);
	} else if (send_ack(socket_fd, peer, peer_length, &frame->header,
			    &server->receive_window)) {
		++server->acknowledgements_sent;
	}
	report_result_if_complete(server);
}

static void process_datagram(struct ivc_server *server, int socket_fd,
			     const struct sockaddr *peer, socklen_t peer_length, size_t length)
{
	struct ivc_decode_rejection rejection;
	struct ivc_frame_view frame;
	struct ivc_control_command command;
	enum ivc_decode_result decode_result;

	replay_ready_if_needed(server);
	decode_result = ivc_decode_frame(receive_frame, length, &frame);
	if (decode_result != IVC_DECODE_OK) {
		if (ivc_decode_rejection_context(receive_frame, length, decode_result,
						 &rejection)) {
			reject_datagram(server, socket_fd, peer, peer_length,
					&rejection.request, rejection.response_error,
					ivc_decode_result_name(decode_result));
		} else {
			++server->protocol_errors;
			printk("IVC-RTOS-DROP reason=%s length=%u\n",
			       ivc_decode_result_name(decode_result), (unsigned int)length);
		}
		return;
	}
	if (frame.header.message_type != IVC_MESSAGE_CONTROL) {
		reject_datagram(server, socket_fd, peer, peer_length, &frame.header,
			       IVC_ERROR_INVALID_CONTROL, "unexpected-message-type");
		return;
	}
	if (!ivc_decode_control(frame.payload, frame.header.payload_length, &command)) {
		reject_datagram(server, socket_fd, peer, peer_length, &frame.header,
			       IVC_ERROR_INVALID_CONTROL, "invalid-control-payload");
		return;
	}
	process_control(server, socket_fd, peer, peer_length, &frame, &command);
}

static void check_safe_timeout(struct ivc_server *server)
{
	enum ivc_timeout_result result = ivc_endpoint_check_timeout(&server->endpoint, monotonic_us());

	if (result == IVC_TIMEOUT_ENTERED_SAFE_STATE) {
		++server->safe_fallbacks;
		printk("IVC-RTOS-SAFE-FALLBACK reason=controller-timeout actuator_permille=%u "
		       "last_sequence=%u session=%u safe_fallbacks=%llu\n",
		       (unsigned int)server->endpoint.actuator_permille,
		       server->endpoint.last_sequence, server->receive_window.session_id,
		       server->safe_fallbacks);
	} else if (result == IVC_TIMEOUT_CLOCK_MOVED_BACKWARD) {
		++server->protocol_errors;
		printk("IVC-RTOS-ERROR seq=%u code=%u reason=clock-moved-backward\n",
		       server->endpoint.last_sequence, (unsigned int)IVC_ERROR_INTERNAL);
	}
}

int main(void)
{
	struct ivc_server server;
	struct zsock_pollfd poll_descriptor;
	int socket_fd;

	if (!ivc_protocol_self_test()) {
		printk("IVC-RTOS-SELFTEST FAIL vector=rust-wire-v1\n");
		return 1;
	}
	printk("IVC-RTOS-SELFTEST PASS vector=rust-wire-v1\n");
	if (!validate_fault_configuration()) {
		return 1;
	}

	/* NET_CONFIG initializes the static IPv4 address before the application. */
	k_sleep(K_MSEC(100));
	if (!validate_network_identity()) {
		return 1;
	}
	socket_fd = open_endpoint_socket();
	if (socket_fd < 0) {
		return 1;
	}

	ivc_receive_window_init(&server.receive_window);
	ivc_endpoint_init(&server.endpoint);
	ivc_thermal_plant_init(&server.plant);
	ivc_ack_loss_policy_init(&server.ack_loss, (uint32_t)CONFIG_IVC_DROP_ACK_EVERY);
	server.applied_commands = 0U;
	server.protocol_errors = 0U;
	server.status_sent = 0U;
	server.acknowledgements_sent = 0U;
	server.errors_sent = 0U;
	server.safe_fallbacks = 0U;
	server.recoveries = 0U;
	server.stale_status_sent = 0U;
	server.stale_acknowledgements_sent = 0U;
	server.error_evidence_count = 0U;
	server.error_evidence_replayed = false;
	server.ready_replayed = false;
	server.result_reported = false;
	poll_descriptor = (struct zsock_pollfd){
		.fd = socket_fd,
		.events = ZSOCK_POLLIN,
	};
	report_ready();

	for (;;) {
		struct sockaddr_storage peer;
		socklen_t peer_length = sizeof(peer);
		int poll_result = zsock_poll(&poll_descriptor, 1, IVC_SOCKET_POLL_MS);

		if (poll_result == 0) {
			check_safe_timeout(&server);
			continue;
		}
		if (poll_result < 0) {
			printk("IVC-RTOS-RX-ERROR stage=poll errno=%d\n", errno);
			check_safe_timeout(&server);
			continue;
		}
		if ((poll_descriptor.revents & ZSOCK_POLLIN) != 0) {
			ssize_t received = zsock_recvfrom(socket_fd, receive_frame,
						  sizeof(receive_frame), 0,
						  (struct sockaddr *)&peer, &peer_length);

			if (received < 0) {
				printk("IVC-RTOS-RX-ERROR stage=recvfrom errno=%d\n", errno);
			} else {
				process_datagram(&server, socket_fd, (const struct sockaddr *)&peer,
						 peer_length, (size_t)received);
			}
		}
		check_safe_timeout(&server);
	}
	return 0;
}
