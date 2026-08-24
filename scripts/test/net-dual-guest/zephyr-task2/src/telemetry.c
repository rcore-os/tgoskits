#include "telemetry.h"

#include <zephyr/sys/atomic.h>

static atomic_t controls_received;
static atomic_t statuses_sent;
static atomic_t heartbeats_received;

void task2_telemetry_note_control(void)
{
	atomic_inc(&controls_received);
}

void task2_telemetry_note_status(void)
{
	atomic_inc(&statuses_sent);
}

void task2_telemetry_note_heartbeat(void)
{
	atomic_inc(&heartbeats_received);
}

struct task2_telemetry_snapshot task2_telemetry_snapshot(void)
{
	return (struct task2_telemetry_snapshot) {
		.controls_received = (uint32_t)atomic_get(&controls_received),
		.statuses_sent = (uint32_t)atomic_get(&statuses_sent),
		.heartbeats_received = (uint32_t)atomic_get(&heartbeats_received),
	};
}
