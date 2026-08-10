#ifndef TASK2_ICPC_PID_PLANT_H
#define TASK2_ICPC_PID_PLANT_H

#include <stddef.h>
#include <stdint.h>

typedef struct {
    double kp;
    double ki;
    double kd;
    double setpoint;
    double y;
    double u;
    double integral;
    double prev_err;
    double tau;
    double dt;
    uint64_t tick;
} pid_plant_t;

void pid_plant_init(pid_plant_t *plant);
void pid_plant_set_gains(pid_plant_t *plant, double kp, double ki, double kd,
                         double setpoint);
void pid_plant_step(pid_plant_t *plant);
double pid_plant_error(const pid_plant_t *plant);

/* Parses "kp=1.2,ki=0.1,kd=0.05,setpoint=100" — missing keys keep current value. */
int pid_plant_parse_ctrl(const char *payload, size_t len, double *kp, double *ki,
                         double *kd, double *setpoint);

/* Writes "y=..,err=..,kp=..,ki=..,kd=..,tick=.." into buf. Returns bytes written. */
int pid_plant_format_state(const pid_plant_t *plant, char *buf, size_t cap);

#endif /* TASK2_ICPC_PID_PLANT_H */
