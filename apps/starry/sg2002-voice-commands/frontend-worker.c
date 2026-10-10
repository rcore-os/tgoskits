// SPDX-License-Identifier: Apache-2.0
#define _POSIX_C_SOURCE 200809L
#include "frontend-protocol.h"
#include <cviruntime.h>
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
static volatile sig_atomic_t stopping;
static void stop(int sig)
{
    (void)sig;
    stopping = 1;
}
static int receive(void *memory, size_t count, int eof_ok)
{
    size_t done = 0;
    while (done < count && !stopping) {
        ssize_t n = read(0, (char *)memory + done, count - done);
        if (n > 0)
            done += (size_t)n;
        else if (n == 0)
            return done == 0 && eof_ok ? 0 : -1;
        else if (errno != EINTR)
            return -1;
    }
    return done == count ? 1 : -1;
}
static int send_all(int fd, const void *memory, size_t count)
{
    size_t done = 0;
    while (done < count && !stopping) {
        ssize_t n = write(fd, (const char *)memory + done, count - done);
        if (n > 0)
            done += (size_t)n;
        else if (n == 0 || errno != EINTR)
            return -1;
    }
    return done == count ? 0 : -1;
}
int main(int argc, char **argv)
{
    if (argc != 2)
        return 2;
    // Reserve the binary response channel; vendor diagnostics go to stderr.
    int response = dup(STDOUT_FILENO);
    if (response < 0 || dup2(STDERR_FILENO, STDOUT_FILENO) < 0)
        return 1;
    struct sigaction action = {0};
    sigemptyset(&action.sa_mask);
    action.sa_handler = stop;
    if (sigaction(SIGINT, &action, NULL) || sigaction(SIGTERM, &action, NULL))
        return 1;
    action.sa_handler = SIG_IGN;
    if (sigaction(SIGPIPE, &action, NULL))
        return 1;
    CVI_MODEL_HANDLE model = NULL;
    CVI_TENSOR *inputs = NULL, *outputs = NULL, *x = NULL, *cache = NULL, *features = NULL,
               *next = NULL;
    int32_t ni = 0, no = 0;
    int rc, status = 1;
    float *frame = NULL;
    rc = CVI_NN_RegisterModel(argv[1], &model);
    if (rc) {
        fprintf(stderr, "TPU register failed: %d\n", rc);
        return 1;
    }
    rc = CVI_NN_GetInputOutputTensors(model, &inputs, &ni, &outputs, &no);
    if (rc || ni != 2 || no != 2) {
        fputs("cannot obtain TPU frontend tensors\n", stderr);
        goto done;
    }
    for (int i = 0; i < ni; i++) {
        if (inputs[i].fmt != CVI_FMT_FP32)
            goto done;
        if (!strcmp(inputs[i].name, "x"))
            x = inputs + i;
        if (!strcmp(inputs[i].name, "embed_states"))
            cache = inputs + i;
    }
    for (int i = 0; i < no; i++) {
        if (outputs[i].fmt != CVI_FMT_FP32)
            goto done;
        if (!strcmp(outputs[i].name, "/out_norm/Mul_1_output_0_Mul_f32"))
            features = outputs + i;
        if (!strcmp(outputs[i].name, "new_embed_states_Slice_f32"))
            next = outputs + i;
    }
    if (!x || !cache || !features || !next || x->count != VOICE_MEL_COUNT ||
        cache->count != VOICE_CACHE_COUNT || features->count != VOICE_FEATURE_COUNT ||
        next->count != VOICE_CACHE_COUNT) {
        fputs("unexpected TPU frontend tensor contract\n", stderr);
        goto done;
    }
    frame = malloc((VOICE_MEL_COUNT + VOICE_CACHE_COUNT) * sizeof(float));
    if (!frame)
        goto done;
    if (send_all(response, voice_frontend_header, sizeof(voice_frontend_header)))
        goto done;
    for (;;) {
        rc = receive(frame, x->count * sizeof(float), 1);
        if (rc == 0) {
            status = 0;
            break;
        }
        if (rc < 0 || receive(frame + x->count, cache->count * sizeof(float), 0) < 0)
            break;
        // Stage ION mappings through ordinary memory: Starry syscall buffers must
        // have normal user pages, even though user-space memcpy can access ION.
        memcpy(CVI_NN_TensorPtr(x), frame, x->count * sizeof(float));
        memcpy(CVI_NN_TensorPtr(cache), frame + x->count, cache->count * sizeof(float));
        rc = CVI_NN_Forward(model, inputs, ni, outputs, no);
        if (rc) {
            fprintf(stderr, "TPU forward failed: %d\n", rc);
            break;
        }
        memcpy(frame, CVI_NN_TensorPtr(features), features->count * sizeof(float));
        memcpy(frame + features->count, CVI_NN_TensorPtr(next), next->count * sizeof(float));
        if (send_all(response, frame, (features->count + next->count) * sizeof(float)))
            break;
    }
done:
    free(frame);
    close(response);
    if (CVI_NN_CleanupModel(model))
        status = 1;
    return status;
}
