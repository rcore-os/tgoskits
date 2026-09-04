/* Offline-trained tiny MLP weights (equivalent to ONNX export for slow-loop tuning). */
#pragma once

#define TASK3_MLP_IN_DIM 4
#define TASK3_MLP_HIDDEN 8
#define TASK3_MLP_OUT_DIM 3

static const double task3_mlp_w1[][4] = {
    {0.0, 4.0, 0.0, 0.0},
    {0.0, 4.0, 0.0, 0.0},
    {0.0, 0.0, 0.0, 0.0},
    {0.0, 0.0, 0.0, 0.0},
    {0.0, 0.0, 0.0, 0.0},
    {0.0, 0.0, 0.0, 0.0},
    {0.0, 0.0, 0.0, 0.0},
    {0.0, 0.0, 0.0, 0.0},
};
static const double task3_mlp_b1[8] = {-1.0, -0.25, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0};
static const double task3_mlp_w2[][8] = {
    {0.15, 0.08, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0},
    {0.02, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0},
    {0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0},
};
static const double task3_mlp_b2[3] = {0.0, 0.0, 0.0};
