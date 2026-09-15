// Copyright 2026 The TGOSKits Authors
//
// SPDX-License-Identifier: Apache-2.0

#include "control_loop.h"

#include <string.h>

void control_state_init(struct control_state *state) {
    memset(state, 0, sizeof(*state));
    state->setpoint = 0.5f;
    state->measured = 0.2f;
}

void control_step(struct control_state *state, const struct aicp_control_payload *control, uint32_t seq) {
    const float dt = 0.02f;
    state->setpoint = control->target;
    state->mode = control->mode;
    float error = state->setpoint - state->measured;
    state->integral += error * dt;
    float derivative = (error - state->last_error) / dt;
    state->control_output = control->kp * error + control->ki * state->integral +
                            control->kd * derivative + control->feed_forward;
    state->measured += state->control_output * 0.12f;
    state->last_error = error;
    state->applied_seq = seq;
}

void control_status(const struct control_state *state, struct aicp_status_payload *status) {
    status->setpoint = state->setpoint;
    status->measured = state->measured;
    status->control_output = state->control_output;
    status->error = state->setpoint - state->measured;
    status->mode = state->mode;
    status->applied_seq = state->applied_seq;
}
