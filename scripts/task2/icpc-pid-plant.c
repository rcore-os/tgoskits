#include "icpc-pid-plant.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

void pid_plant_init(pid_plant_t *plant)
{
    plant->kp = 0.8;
    plant->ki = 0.05;
    plant->kd = 0.01;
    plant->setpoint = 0.0;
    plant->y = 0.0;
    plant->u = 0.0;
    plant->integral = 0.0;
    plant->prev_err = 0.0;
    plant->tau = 0.08;
    plant->dt = 0.001;
    plant->tick = 0;
}

void pid_plant_set_gains(pid_plant_t *plant, double kp, double ki, double kd,
                         double setpoint)
{
    if (kp >= 0.0)
        plant->kp = kp;
    if (ki >= 0.0)
        plant->ki = ki;
    if (kd >= 0.0)
        plant->kd = kd;
    plant->setpoint = setpoint;
}

void pid_plant_step(pid_plant_t *plant)
{
    double err = plant->setpoint - plant->y;
    plant->integral += err * plant->dt;
    double deriv = (err - plant->prev_err) / plant->dt;
    plant->u = plant->kp * err + plant->ki * plant->integral + plant->kd * deriv;
    if (plant->u > 100.0)
        plant->u = 100.0;
    if (plant->u < 0.0)
        plant->u = 0.0;

    double dy = (plant->u - plant->y) / plant->tau;
    plant->y += dy * plant->dt;
    plant->prev_err = err;
    plant->tick++;
}

double pid_plant_error(const pid_plant_t *plant)
{
    return plant->setpoint - plant->y;
}

static int parse_key_double(const char *payload, size_t len, const char *key,
                            double *out)
{
    char tmp[128];
    if (len >= sizeof(tmp))
        len = sizeof(tmp) - 1;
    memcpy(tmp, payload, len);
    tmp[len] = '\0';

    char pattern[32];
    snprintf(pattern, sizeof(pattern), "%s=", key);
    char *start = strstr(tmp, pattern);
    if (!start)
        return 0;
    start += strlen(pattern);
    char *end = start;
    while (*end && *end != ',' && *end != ' ')
        end++;
    *end = '\0';
    *out = strtod(start, NULL);
    return 1;
}

int pid_plant_parse_ctrl(const char *payload, size_t len, double *kp, double *ki,
                         double *kd, double *setpoint)
{
    int found = 0;
    double v;

    if (parse_key_double(payload, len, "kp", &v)) {
        *kp = v;
        found = 1;
    }
    if (parse_key_double(payload, len, "ki", &v)) {
        *ki = v;
        found = 1;
    }
    if (parse_key_double(payload, len, "kd", &v)) {
        *kd = v;
        found = 1;
    }
    if (parse_key_double(payload, len, "setpoint", &v)) {
        *setpoint = v;
        found = 1;
    }
    return found;
}

int pid_plant_format_state(const pid_plant_t *plant, char *buf, size_t cap)
{
    double err = pid_plant_error(plant);
    return snprintf(buf, cap, "y=%.3f,err=%.3f,kp=%.3f,ki=%.3f,kd=%.3f,tick=%llu",
                    plant->y, err, plant->kp, plant->ki, plant->kd,
                    (unsigned long long)plant->tick);
}
