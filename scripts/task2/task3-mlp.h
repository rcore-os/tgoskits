#pragma once

#include "task3-mlp-weights.h"

#include <math.h>

static inline double task3_mlp_relu(double x) { return x > 0.0 ? x : 0.0; }

static inline void task3_mlp_forward(double err, double kp, double ki, double out[3])
{
    const double x[TASK3_MLP_IN_DIM] = {
        err / 80.0,
        fabs(err) / 80.0,
        kp / 8.0,
        ki / 0.5,
    };
    double hidden[TASK3_MLP_HIDDEN];
    for (int i = 0; i < TASK3_MLP_HIDDEN; i++) {
        double sum = task3_mlp_b1[i];
        for (int j = 0; j < TASK3_MLP_IN_DIM; j++)
            sum += task3_mlp_w1[i][j] * x[j];
        hidden[i] = task3_mlp_relu(sum);
    }
    for (int i = 0; i < TASK3_MLP_OUT_DIM; i++) {
        double sum = task3_mlp_b2[i];
        for (int j = 0; j < TASK3_MLP_HIDDEN; j++)
            sum += task3_mlp_w2[i][j] * hidden[j];
        out[i] = sum;
    }
}
