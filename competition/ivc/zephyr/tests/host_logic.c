/* SPDX-License-Identifier: Apache-2.0 */

#include "endpoint.h"
#include "protocol.h"

#include <assert.h>
#include <stdint.h>
#include <stdio.h>

static struct ivc_control_command neural_command(uint32_t sample_id)
{
	return (struct ivc_control_command){
		.operation = IVC_CONTROL_SET_ACTUATOR,
		.mode = IVC_MODE_NEURAL,
		.actuator_permille = 1000,
		.setpoint_milli_c = 55000,
		.sample_id = sample_id,
	};
}

static void test_protocol_golden_vector(void)
{
	assert(ivc_protocol_self_test());
}

static void test_crc32_bytes_matches_the_standard_check_value(void)
{
	static const uint8_t check_value[] = "123456789";

	assert(ivc_crc32_bytes(check_value, sizeof(check_value) - 1U) == UINT32_C(0xcbf43926));
}

static void test_decode_failures_preserve_safe_error_response_context(void)
{
	const struct ivc_header header = {
		.message_type = IVC_MESSAGE_CONTROL,
		.flags = IVC_FLAG_ACK_REQUIRED,
		.session_id = UINT32_C(0x4354524c),
		.sequence = 7U,
		.timestamp_us = UINT64_C(1234),
		.payload_length = 1U,
		.error_code = IVC_ERROR_NONE,
	};
	const uint8_t payload[] = {0x5a};
	struct ivc_decode_rejection rejection;
	uint8_t frame[IVC_MAX_FRAME_LENGTH];
	size_t frame_length;

	assert(ivc_encode_frame(&header, payload, frame, sizeof(frame), &frame_length));
	frame[4] = IVC_PROTOCOL_VERSION + 1U;
	assert(ivc_decode_frame(frame, frame_length, &(struct ivc_frame_view){0}) ==
	       IVC_DECODE_UNSUPPORTED_VERSION);
	assert(ivc_decode_rejection_context(frame, frame_length,
					    IVC_DECODE_UNSUPPORTED_VERSION, &rejection));
	assert(rejection.response_error == IVC_ERROR_UNSUPPORTED_VERSION);
	assert(rejection.request.session_id == header.session_id);
	assert(rejection.request.sequence == header.sequence);

	assert(ivc_encode_frame(&header, payload, frame, sizeof(frame), &frame_length));
	frame[24] = 2U;
	assert(ivc_decode_rejection_context(frame, frame_length, IVC_DECODE_LENGTH_MISMATCH,
					    &rejection));
	assert(rejection.response_error == IVC_ERROR_MALFORMED_FRAME);

	assert(ivc_encode_frame(&header, payload, frame, sizeof(frame), &frame_length));
	frame[frame_length - 1U] ^= 1U;
	assert(ivc_decode_rejection_context(frame, frame_length,
					    IVC_DECODE_CHECKSUM_MISMATCH, &rejection));
	assert(rejection.response_error == IVC_ERROR_CHECKSUM_MISMATCH);

	frame[0] = 0U;
	assert(!ivc_decode_rejection_context(frame, frame_length, IVC_DECODE_BAD_MAGIC,
					     &rejection));
}

static void test_receive_window_exact_once_and_reordering(void)
{
	struct ivc_receive_window window;

	ivc_receive_window_init(&window);
	assert(ivc_receive_window_observe(&window, UINT32_C(0x4354524c), 1) ==
	       IVC_DELIVERY_NEW_SESSION);
	assert(ivc_receive_window_observe(&window, UINT32_C(0x4354524c), 3) ==
	       IVC_DELIVERY_NEW_OUT_OF_ORDER);
	assert(ivc_receive_window_observe(&window, UINT32_C(0x4354524c), 3) ==
	       IVC_DELIVERY_DUPLICATE);
	assert(ivc_receive_window_observe(&window, UINT32_C(0x4354524c), 2) ==
	       IVC_DELIVERY_NEW_IN_ORDER);
	assert(window.metrics.accepted == 3);
	assert(window.metrics.reordered == 1);
	assert(window.metrics.duplicates == 1);
}

static void test_controller_restart_resets_replay_state_without_session_rollback(void)
{
	struct ivc_receive_window window;
	struct ivc_endpoint endpoint;
	const struct ivc_control_command first = neural_command(1);
	const struct ivc_control_command restarted = neural_command(1);
	const uint32_t first_session = UINT32_C(0x4354524c);
	const uint32_t restarted_session = UINT32_C(0x4354524d);

	ivc_receive_window_init(&window);
	ivc_endpoint_init(&endpoint);
	assert(ivc_receive_window_observe(&window, first_session, 1) ==
	       IVC_DELIVERY_NEW_SESSION);
	ivc_endpoint_begin_session(&endpoint);
	assert(ivc_endpoint_apply(&endpoint, 1, &first, 1000, 1000) == IVC_APPLY_APPLIED);

	assert(ivc_receive_window_observe(&window, restarted_session, 1) ==
	       IVC_DELIVERY_NEW_SESSION);
	ivc_endpoint_begin_session(&endpoint);
	assert(ivc_endpoint_apply(&endpoint, 1, &restarted, 2000, 2000) ==
	       IVC_APPLY_APPLIED);
	assert(ivc_receive_window_observe(&window, first_session, 2) ==
	       IVC_DELIVERY_SESSION_REJECTED);
	assert(ivc_receive_window_observe(&window, UINT32_C(0x4354524e), 2) ==
	       IVC_DELIVERY_SESSION_REJECTED);
	assert(window.session_id == restarted_session);
	assert(window.metrics.session_resets == 1);
	assert(window.metrics.session_rejections == 2);
}

static void test_endpoint_rejects_replay_and_stale_time(void)
{
	struct ivc_endpoint endpoint;
	const struct ivc_control_command first = neural_command(1);
	const struct ivc_control_command second = neural_command(2);

	ivc_endpoint_init(&endpoint);
	assert(ivc_endpoint_apply(&endpoint, 1, &first, 1000, 1000) == IVC_APPLY_APPLIED);
	assert(ivc_endpoint_apply(&endpoint, 1, &first, 1000, 1000) ==
	       IVC_APPLY_STALE_SEQUENCE);
	assert(ivc_endpoint_apply(&endpoint, 2, &second, 1000, 251001) ==
	       IVC_APPLY_STALE_TIMESTAMP);
	assert(endpoint.last_sequence == 1);
	assert(endpoint.actuator_permille == 1000);
}

static void test_timeout_boundary_enters_safe_state_once(void)
{
	struct ivc_endpoint endpoint;
	const struct ivc_control_command command = neural_command(1);

	ivc_endpoint_init(&endpoint);
	assert(ivc_endpoint_apply(&endpoint, 1, &command, 1000, 1000) == IVC_APPLY_APPLIED);
	assert(ivc_endpoint_check_timeout(&endpoint, 501000) == IVC_TIMEOUT_NO_CHANGE);
	assert(ivc_endpoint_check_timeout(&endpoint, 501001) ==
	       IVC_TIMEOUT_ENTERED_SAFE_STATE);
	assert(ivc_endpoint_check_timeout(&endpoint, 501002) == IVC_TIMEOUT_NO_CHANGE);
	assert(endpoint.actuator_permille == IVC_SAFE_ACTUATOR_PERMILLE);
	assert(endpoint.fault == IVC_ERROR_CONTROLLER_TIMEOUT);
}

static void test_fresh_restart_session_recovers_from_controller_timeout(void)
{
	struct ivc_receive_window window;
	struct ivc_endpoint endpoint;
	const struct ivc_control_command first = neural_command(20);
	const struct ivc_control_command restarted = neural_command(1);
	const uint32_t first_session = UINT32_C(0x11111111);
	const uint32_t restarted_session = UINT32_C(0x22222222);

	ivc_receive_window_init(&window);
	ivc_endpoint_init(&endpoint);
	assert(ivc_receive_window_observe(&window, first_session, 1) ==
	       IVC_DELIVERY_NEW_SESSION);
	ivc_endpoint_begin_session(&endpoint);
	assert(ivc_endpoint_apply(&endpoint, 1, &first, 1000, 1000) == IVC_APPLY_APPLIED);
	assert(ivc_endpoint_check_timeout(&endpoint, 501001) ==
	       IVC_TIMEOUT_ENTERED_SAFE_STATE);

	assert(ivc_receive_window_observe(&window, restarted_session, 1) ==
	       IVC_DELIVERY_NEW_SESSION);
	ivc_endpoint_begin_session(&endpoint);
	assert(ivc_endpoint_apply(&endpoint, 1, &restarted, 600000, 600000) ==
	       IVC_APPLY_APPLIED);
	assert(endpoint.fault == IVC_ERROR_NONE);
	assert(endpoint.active_mode == IVC_MODE_NEURAL);
	assert(endpoint.actuator_permille == restarted.actuator_permille);
	assert(window.metrics.session_resets == 1U);
	assert(ivc_receive_window_observe(&window, first_session, 21) ==
	       IVC_DELIVERY_SESSION_REJECTED);
}

static void test_ack_loss_policy_drops_only_the_first_ack_for_selected_fresh_commands(void)
{
	struct ivc_ack_loss_policy disabled;
	struct ivc_ack_loss_policy every_fifth;

	ivc_ack_loss_policy_init(&disabled, 0U);
	assert(!ivc_ack_loss_policy_should_drop(&disabled, IVC_DELIVERY_NEW_IN_ORDER, 5U));
	assert(disabled.acknowledgements_dropped == 0U);

	ivc_ack_loss_policy_init(&every_fifth, 5U);
	assert(!ivc_ack_loss_policy_should_drop(&every_fifth, IVC_DELIVERY_NEW_IN_ORDER, 4U));
	assert(ivc_ack_loss_policy_should_drop(&every_fifth, IVC_DELIVERY_NEW_IN_ORDER, 5U));
	assert(!ivc_ack_loss_policy_should_drop(&every_fifth, IVC_DELIVERY_DUPLICATE, 5U));
	assert(ivc_ack_loss_policy_should_drop(&every_fifth, IVC_DELIVERY_NEW_IN_ORDER, 10U));
	assert(every_fifth.acknowledgements_dropped == 2U);
}

static void test_ack_loss_retransmission_does_not_repeat_the_plant_step(void)
{
	struct ivc_receive_window window;
	struct ivc_endpoint endpoint;
	struct ivc_thermal_plant plant;
	struct ivc_ack_loss_policy policy;
	enum ivc_delivery delivery;
	int32_t temperature_after_fresh_command;
	uint32_t sequence;

	ivc_receive_window_init(&window);
	ivc_endpoint_init(&endpoint);
	ivc_thermal_plant_init(&plant);
	ivc_ack_loss_policy_init(&policy, 5U);

	for (sequence = 1U; sequence <= 5U; ++sequence) {
		const struct ivc_control_command command = neural_command(sequence);
		const uint64_t received_us = (uint64_t)sequence * UINT64_C(1000);

		delivery = ivc_receive_window_observe(&window, UINT32_C(0x4354524c), sequence);
		assert(delivery == (sequence == 1U ? IVC_DELIVERY_NEW_SESSION :
						       IVC_DELIVERY_NEW_IN_ORDER));
		assert(ivc_delivery_applies_control(delivery));
		if (delivery == IVC_DELIVERY_NEW_SESSION) {
			ivc_endpoint_begin_session(&endpoint);
		}
		assert(ivc_endpoint_apply(&endpoint, sequence, &command, received_us, received_us) ==
		       IVC_APPLY_APPLIED);
		ivc_thermal_plant_step(&plant, endpoint.actuator_permille, sequence - 1U);
		assert(ivc_ack_loss_policy_should_drop(&policy, delivery, sequence) ==
		       (sequence == 5U));
	}
	temperature_after_fresh_command = ivc_thermal_plant_temperature(&plant);

	delivery = ivc_receive_window_observe(&window, UINT32_C(0x4354524c), 5U);
	assert(delivery == IVC_DELIVERY_DUPLICATE);
	assert(!ivc_delivery_applies_control(delivery));
	assert(!ivc_ack_loss_policy_should_drop(&policy, delivery, 5U));
	if (ivc_delivery_applies_control(delivery)) {
		ivc_thermal_plant_step(&plant, endpoint.actuator_permille, 1U);
	}
	assert(ivc_thermal_plant_temperature(&plant) == temperature_after_fresh_command);
	assert(endpoint.last_sequence == 5U);
	assert(policy.acknowledgements_dropped == 1U);
	assert(window.metrics.accepted == 5U);
	assert(window.metrics.duplicates == 1U);
}

static void test_thermal_plant_step_matches_rust_reference(void)
{
	struct ivc_thermal_plant plant;

	ivc_thermal_plant_init(&plant);
	ivc_thermal_plant_step(&plant, 1000, 0);
	assert(ivc_thermal_plant_temperature(&plant) == 20280);
}

int main(void)
{
	test_protocol_golden_vector();
	test_crc32_bytes_matches_the_standard_check_value();
	test_decode_failures_preserve_safe_error_response_context();
	test_receive_window_exact_once_and_reordering();
	test_controller_restart_resets_replay_state_without_session_rollback();
	test_endpoint_rejects_replay_and_stale_time();
	test_timeout_boundary_enters_safe_state_once();
	test_fresh_restart_session_recovers_from_controller_timeout();
	test_ack_loss_policy_drops_only_the_first_ack_for_selected_fresh_commands();
	test_ack_loss_retransmission_does_not_repeat_the_plant_step();
	test_thermal_plant_step_matches_rust_reference();
	puts("host-logic-tests: PASS");
	return 0;
}
