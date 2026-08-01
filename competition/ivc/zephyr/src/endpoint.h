/* SPDX-License-Identifier: Apache-2.0 */

#ifndef IVC_ENDPOINT_H_
#define IVC_ENDPOINT_H_

#include "protocol.h"

#include <stdbool.h>
#include <stdint.h>

#define IVC_RECEIVE_WINDOW_BITS 64U
#define IVC_RETIRED_SESSION_CAPACITY 8U
#define IVC_SAFE_ACTUATOR_PERMILLE 0U
#define IVC_COMMAND_TIMEOUT_US UINT64_C(500000)
#define IVC_MAXIMUM_COMMAND_AGE_US UINT64_C(250000)
#define IVC_INITIAL_SETPOINT_MILLI_C 45000
#define IVC_INITIAL_TEMPERATURE_MILLI_C 20000

struct ivc_receiver_metrics {
	uint64_t accepted;
	uint64_t duplicates;
	uint64_t reordered;
	uint64_t outside_window;
	uint64_t session_resets;
	uint64_t session_rejections;
};

struct ivc_receive_window {
	bool has_session;
	uint32_t session_id;
	uint32_t next_sequence;
	uint64_t received_mask;
	uint32_t retired_sessions[IVC_RETIRED_SESSION_CAPACITY];
	uint8_t retired_session_count;
	uint8_t retired_session_cursor;
	struct ivc_receiver_metrics metrics;
};

enum ivc_delivery {
	IVC_DELIVERY_NEW_SESSION,
	IVC_DELIVERY_NEW_IN_ORDER,
	IVC_DELIVERY_NEW_OUT_OF_ORDER,
	IVC_DELIVERY_DUPLICATE,
	IVC_DELIVERY_OUTSIDE_WINDOW,
	IVC_DELIVERY_INVALID_IDENTIFIER,
	IVC_DELIVERY_SEQUENCE_EXHAUSTED,
	IVC_DELIVERY_SESSION_REJECTED,
};

struct ivc_ack_loss_policy {
	uint32_t drop_every;
	uint64_t acknowledgements_dropped;
};

struct ivc_endpoint {
	enum ivc_control_mode active_mode;
	uint16_t actuator_permille;
	int32_t setpoint_milli_c;
	uint32_t last_sequence;
	bool has_last_sample;
	uint32_t last_sample_id;
	bool has_last_valid_command;
	uint64_t last_valid_command_us;
	enum ivc_error_code fault;
};

enum ivc_apply_result {
	IVC_APPLY_APPLIED,
	IVC_APPLY_ENTERED_SAFE_STATE,
	IVC_APPLY_INVALID_PAYLOAD,
	IVC_APPLY_FUTURE_TIMESTAMP,
	IVC_APPLY_STALE_TIMESTAMP,
	IVC_APPLY_STALE_SEQUENCE,
	IVC_APPLY_STALE_SAMPLE,
};

enum ivc_timeout_result {
	IVC_TIMEOUT_NO_CHANGE,
	IVC_TIMEOUT_ENTERED_SAFE_STATE,
	IVC_TIMEOUT_CLOCK_MOVED_BACKWARD,
};

struct ivc_thermal_plant {
	float temperature_c;
};

void ivc_receive_window_init(struct ivc_receive_window *window);

enum ivc_delivery ivc_receive_window_observe(struct ivc_receive_window *window,
					      uint32_t session_id, uint32_t sequence);

bool ivc_delivery_applies_control(enum ivc_delivery delivery);

struct ivc_ack_payload ivc_receive_window_ack(const struct ivc_receive_window *window,
					      uint32_t acknowledged_sequence);

void ivc_ack_loss_policy_init(struct ivc_ack_loss_policy *policy, uint32_t drop_every);

bool ivc_ack_loss_policy_should_drop(struct ivc_ack_loss_policy *policy,
				     enum ivc_delivery delivery, uint32_t sequence);

void ivc_endpoint_init(struct ivc_endpoint *endpoint);

void ivc_endpoint_begin_session(struct ivc_endpoint *endpoint);

enum ivc_apply_result ivc_endpoint_apply(struct ivc_endpoint *endpoint, uint32_t sequence,
					 const struct ivc_control_command *command,
					 uint64_t sent_at_us, uint64_t now_us);

enum ivc_timeout_result ivc_endpoint_check_timeout(struct ivc_endpoint *endpoint,
						   uint64_t now_us);

struct ivc_status_report ivc_endpoint_status(const struct ivc_endpoint *endpoint,
					     int32_t measured_milli_c);

const char *ivc_control_mode_name(enum ivc_control_mode mode);
const char *ivc_apply_result_name(enum ivc_apply_result result);

void ivc_thermal_plant_init(struct ivc_thermal_plant *plant);
void ivc_thermal_plant_step(struct ivc_thermal_plant *plant, uint16_t actuator_permille,
			    uint32_t step);
int32_t ivc_thermal_plant_temperature(const struct ivc_thermal_plant *plant);

#endif /* IVC_ENDPOINT_H_ */
