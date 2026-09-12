// Copyright 2026 The TGOSKits Authors
//
// SPDX-License-Identifier: Apache-2.0

#ifndef TGOSKITS_AI_RTOS_DEMO_CONTROL_LOOP_H
#define TGOSKITS_AI_RTOS_DEMO_CONTROL_LOOP_H

#include "aicp.h"

struct control_state {
    float setpoint;
    float measured;
    float integral;
    float last_error;
    float control_output;
    uint32_t mode;
    uint32_t applied_seq;
};

void control_state_init(struct control_state *state);
void control_step(struct control_state *state, const struct aicp_control_payload *control, uint32_t seq);
void control_status(const struct control_state *state, struct aicp_status_payload *status);

#endif
