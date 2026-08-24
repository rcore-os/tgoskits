#ifndef TASK2_TELEMETRY_H_
#define TASK2_TELEMETRY_H_

#include <stdint.h>

struct task2_telemetry_snapshot {
	uint32_t controls_received;
	uint32_t statuses_sent;
	uint32_t heartbeats_received;
};

void task2_telemetry_note_control(void);
void task2_telemetry_note_status(void);
void task2_telemetry_note_heartbeat(void);
struct task2_telemetry_snapshot task2_telemetry_snapshot(void);

#endif /* TASK2_TELEMETRY_H_ */
