/* SPDX-License-Identifier: Apache-2.0 */

#include "endpoint.h"

#include <limits.h>

static bool control_valid(const struct ivc_control_command *command)
{
	if (command == NULL || command->operation < IVC_CONTROL_SET_ACTUATOR ||
	    command->operation > IVC_CONTROL_HEARTBEAT || command->mode < IVC_MODE_SAFE ||
	    command->mode > IVC_MODE_NEURAL ||
	    command->actuator_permille > IVC_MAX_ACTUATOR_PERMILLE ||
	    command->setpoint_milli_c < IVC_MIN_TEMPERATURE_MILLI_C ||
	    command->setpoint_milli_c > IVC_MAX_TEMPERATURE_MILLI_C) {
		return false;
	}
	return command->operation != IVC_CONTROL_ENTER_SAFE_STATE || command->mode == IVC_MODE_SAFE;
}

static void enter_safe_state(struct ivc_endpoint *endpoint, enum ivc_error_code fault)
{
	endpoint->active_mode = IVC_MODE_SAFE;
	endpoint->actuator_permille = IVC_SAFE_ACTUATOR_PERMILLE;
	endpoint->fault = fault;
}

static bool session_retired(const struct ivc_receive_window *window, uint32_t session_id)
{
	uint8_t index;

	for (index = 0U; index < window->retired_session_count; ++index) {
		if (window->retired_sessions[index] == session_id) {
			return true;
		}
	}
	return false;
}

static void retire_session(struct ivc_receive_window *window, uint32_t session_id)
{
	window->retired_sessions[window->retired_session_cursor] = session_id;
	window->retired_session_cursor =
		(uint8_t)((window->retired_session_cursor + 1U) % IVC_RETIRED_SESSION_CAPACITY);
	if (window->retired_session_count < IVC_RETIRED_SESSION_CAPACITY) {
		++window->retired_session_count;
	}
}

void ivc_receive_window_init(struct ivc_receive_window *window)
{
	*window = (struct ivc_receive_window){
		.next_sequence = 1U,
	};
}

enum ivc_delivery ivc_receive_window_observe(struct ivc_receive_window *window,
					      uint32_t session_id, uint32_t sequence)
{
	uint32_t offset;
	uint64_t bit;
	bool new_session = false;

	if (window == NULL || session_id == 0U || sequence == 0U) {
		return IVC_DELIVERY_INVALID_IDENTIFIER;
	}
	if (!window->has_session) {
		new_session = true;
	} else if (window->session_id != session_id) {
		if (sequence != 1U || session_retired(window, session_id)) {
			++window->metrics.session_rejections;
			return IVC_DELIVERY_SESSION_REJECTED;
		}
		retire_session(window, window->session_id);
		++window->metrics.session_resets;
		new_session = true;
	}
	if (new_session) {
		window->has_session = true;
		window->session_id = session_id;
		window->next_sequence = 1U;
		window->received_mask = 0U;
	}
	if (sequence < window->next_sequence) {
		++window->metrics.duplicates;
		return IVC_DELIVERY_DUPLICATE;
	}
	offset = sequence - window->next_sequence;
	if (offset >= IVC_RECEIVE_WINDOW_BITS) {
		++window->metrics.outside_window;
		return IVC_DELIVERY_OUTSIDE_WINDOW;
	}
	bit = UINT64_C(1) << offset;
	if ((window->received_mask & bit) != 0U) {
		++window->metrics.duplicates;
		return IVC_DELIVERY_DUPLICATE;
	}
	window->received_mask |= bit;
	++window->metrics.accepted;
	if (offset != 0U) {
		++window->metrics.reordered;
	}
	while ((window->received_mask & UINT64_C(1)) != 0U) {
		window->received_mask >>= 1;
		if (window->next_sequence == UINT32_MAX) {
			return IVC_DELIVERY_SEQUENCE_EXHAUSTED;
		}
		++window->next_sequence;
	}
	if (new_session) {
		return offset == 0U ? IVC_DELIVERY_NEW_SESSION : IVC_DELIVERY_NEW_OUT_OF_ORDER;
	}
	return offset == 0U ? IVC_DELIVERY_NEW_IN_ORDER : IVC_DELIVERY_NEW_OUT_OF_ORDER;
}

bool ivc_delivery_applies_control(enum ivc_delivery delivery)
{
	return delivery == IVC_DELIVERY_NEW_SESSION || delivery == IVC_DELIVERY_NEW_IN_ORDER;
}

struct ivc_ack_payload ivc_receive_window_ack(const struct ivc_receive_window *window,
					      uint32_t acknowledged_sequence)
{
	return (struct ivc_ack_payload){
		.acknowledged_sequence = acknowledged_sequence,
		.next_expected_sequence = window->next_sequence,
		/* The Rust wire contract intentionally exports only the low 32 bits. */
		.received_mask = (uint32_t)window->received_mask,
	};
}

void ivc_ack_loss_policy_init(struct ivc_ack_loss_policy *policy, uint32_t drop_every)
{
	*policy = (struct ivc_ack_loss_policy){
		.drop_every = drop_every,
	};
}

bool ivc_ack_loss_policy_should_drop(struct ivc_ack_loss_policy *policy,
				     enum ivc_delivery delivery, uint32_t sequence)
{
	bool fresh;

	if (policy == NULL || policy->drop_every == 0U || sequence == 0U) {
		return false;
	}
	fresh = ivc_delivery_applies_control(delivery);
	if (!fresh || (sequence % policy->drop_every) != 0U) {
		return false;
	}
	++policy->acknowledgements_dropped;
	return true;
}

void ivc_endpoint_init(struct ivc_endpoint *endpoint)
{
	*endpoint = (struct ivc_endpoint){
		.active_mode = IVC_MODE_SAFE,
		.actuator_permille = IVC_SAFE_ACTUATOR_PERMILLE,
		.setpoint_milli_c = IVC_INITIAL_SETPOINT_MILLI_C,
		.fault = IVC_ERROR_NONE,
	};
}

void ivc_endpoint_begin_session(struct ivc_endpoint *endpoint)
{
	endpoint->last_sequence = 0U;
	endpoint->has_last_sample = false;
	endpoint->last_sample_id = 0U;
}

enum ivc_apply_result ivc_endpoint_apply(struct ivc_endpoint *endpoint, uint32_t sequence,
					 const struct ivc_control_command *command,
					 uint64_t sent_at_us, uint64_t now_us)
{
	uint64_t age_us;

	if (endpoint == NULL || !control_valid(command)) {
		return IVC_APPLY_INVALID_PAYLOAD;
	}
	if (now_us < sent_at_us) {
		return IVC_APPLY_FUTURE_TIMESTAMP;
	}
	age_us = now_us - sent_at_us;
	if (age_us > IVC_MAXIMUM_COMMAND_AGE_US) {
		return IVC_APPLY_STALE_TIMESTAMP;
	}
	if (sequence <= endpoint->last_sequence) {
		return IVC_APPLY_STALE_SEQUENCE;
	}
	if (endpoint->has_last_sample && command->sample_id <= endpoint->last_sample_id) {
		return IVC_APPLY_STALE_SAMPLE;
	}

	endpoint->last_sequence = sequence;
	endpoint->has_last_sample = true;
	endpoint->last_sample_id = command->sample_id;
	endpoint->has_last_valid_command = true;
	endpoint->last_valid_command_us = now_us;
	endpoint->setpoint_milli_c = command->setpoint_milli_c;
	endpoint->fault = IVC_ERROR_NONE;
	if (command->operation == IVC_CONTROL_ENTER_SAFE_STATE) {
		enter_safe_state(endpoint, IVC_ERROR_NONE);
		return IVC_APPLY_ENTERED_SAFE_STATE;
	}
	endpoint->active_mode = command->mode;
	endpoint->actuator_permille = command->actuator_permille;
	return IVC_APPLY_APPLIED;
}

enum ivc_timeout_result ivc_endpoint_check_timeout(struct ivc_endpoint *endpoint,
						   uint64_t now_us)
{
	uint64_t silence_us;

	if (endpoint == NULL || !endpoint->has_last_valid_command) {
		return IVC_TIMEOUT_NO_CHANGE;
	}
	if (now_us < endpoint->last_valid_command_us) {
		return IVC_TIMEOUT_CLOCK_MOVED_BACKWARD;
	}
	silence_us = now_us - endpoint->last_valid_command_us;
	if (silence_us <= IVC_COMMAND_TIMEOUT_US) {
		return IVC_TIMEOUT_NO_CHANGE;
	}
	if (endpoint->active_mode == IVC_MODE_SAFE &&
	    endpoint->actuator_permille == IVC_SAFE_ACTUATOR_PERMILLE &&
	    endpoint->fault == IVC_ERROR_CONTROLLER_TIMEOUT) {
		return IVC_TIMEOUT_NO_CHANGE;
	}
	enter_safe_state(endpoint, IVC_ERROR_CONTROLLER_TIMEOUT);
	return IVC_TIMEOUT_ENTERED_SAFE_STATE;
}

struct ivc_status_report ivc_endpoint_status(const struct ivc_endpoint *endpoint,
					     int32_t measured_milli_c)
{
	enum ivc_status_state state;

	if (endpoint->fault == IVC_ERROR_CONTROLLER_TIMEOUT) {
		state = IVC_STATUS_SAFE_FALLBACK;
	} else if (endpoint->last_sequence == 0U) {
		state = IVC_STATUS_READY;
	} else {
		state = IVC_STATUS_APPLIED;
	}
	return (struct ivc_status_report){
		.state = state,
		.active_mode = endpoint->active_mode,
		.actuator_permille = endpoint->actuator_permille,
		.measured_milli_c = measured_milli_c,
		.setpoint_milli_c = endpoint->setpoint_milli_c,
		.applied_sequence = endpoint->last_sequence,
		.fault = endpoint->fault,
	};
}

const char *ivc_control_mode_name(enum ivc_control_mode mode)
{
	switch (mode) {
	case IVC_MODE_SAFE:
		return "Safe";
	case IVC_MODE_MANUAL_FIXED:
		return "ManualFixed";
	case IVC_MODE_NEURAL:
		return "Neural";
	default:
		return "Unknown";
	}
}

const char *ivc_apply_result_name(enum ivc_apply_result result)
{
	switch (result) {
	case IVC_APPLY_APPLIED:
		return "applied";
	case IVC_APPLY_ENTERED_SAFE_STATE:
		return "entered-safe-state";
	case IVC_APPLY_INVALID_PAYLOAD:
		return "invalid-payload";
	case IVC_APPLY_FUTURE_TIMESTAMP:
		return "future-timestamp";
	case IVC_APPLY_STALE_TIMESTAMP:
		return "stale-timestamp";
	case IVC_APPLY_STALE_SEQUENCE:
		return "stale-sequence";
	case IVC_APPLY_STALE_SAMPLE:
		return "stale-sample";
	default:
		return "unknown-apply-result";
	}
}

void ivc_thermal_plant_init(struct ivc_thermal_plant *plant)
{
	plant->temperature_c = (float)IVC_INITIAL_TEMPERATURE_MILLI_C / 1000.0F;
}

void ivc_thermal_plant_step(struct ivc_thermal_plant *plant, uint16_t actuator_permille,
			    uint32_t step)
{
	const float ambient_c = 20.0F;
	const float heater_c_per_s = 2.8F;
	const float cooling_per_s = 0.04F;
	const float dt_s = 0.1F;
	const float actuator = (float)actuator_permille / 1000.0F;
	const float disturbance = step >= 850U && step < 950U ? -0.35F : 0.0F;
	const float derivative = heater_c_per_s * actuator -
				 cooling_per_s * (plant->temperature_c - ambient_c) + disturbance;

	plant->temperature_c += derivative * dt_s;
}

int32_t ivc_thermal_plant_temperature(const struct ivc_thermal_plant *plant)
{
	float scaled = plant->temperature_c * 1000.0F;

	return (int32_t)(scaled >= 0.0F ? scaled + 0.5F : scaled - 0.5F);
}
