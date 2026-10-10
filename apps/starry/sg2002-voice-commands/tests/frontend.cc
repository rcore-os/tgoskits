// SPDX-License-Identifier: Apache-2.0
#include <array>
#include <cmath>
#include <csignal>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <onnxruntime_cxx_api.h>
#include <sys/wait.h>
#include <vector>
int main(int argc, char **argv)
{
    if (argc != 5)
        return 2;
    std::signal(SIGPIPE, SIG_IGN);
    try {
        setenv("VOICE_FRONTEND_MODEL", argv[1], 1);
        setenv("VOICE_FRONTEND_WORKER", argv[4], 1);
        Ort::Env env(ORT_LOGGING_LEVEL_FATAL, "bridge-test");
        Ort::SessionOptions options;
        options.SetIntraOpNumThreads(1);
        options.SetInterOpNumThreads(1);
        Ort::Session reference(env, argv[1], options);
        options.RegisterCustomOpsLibrary(argv[3]);
        const char *names[] = {"x", "embed_states"},
                   *outputs[] = {"/out_norm/Mul_1_output_0", "new_embed_states"};
        auto memory = Ort::MemoryInfo::CreateCpu(OrtArenaAllocator, OrtMemTypeDefault);
        std::vector<float> x(3600), cache(7296);
        int64_t xs[] = {1, 45, 80}, cs[] = {1, 128, 3, 19};
        {
            Ort::Session candidate(env, argv[2], options);
            for (int step = 0; step < 5; step++) {
                for (size_t i = 0; i < x.size(); i++)
                    x[i] = static_cast<float>(std::sin((i + step * 3600) * .03) * 2 - 3);
                std::array<Ort::Value, 2> inputs = {
                    Ort::Value::CreateTensor<float>(memory, x.data(), x.size(), xs, 3),
                    Ort::Value::CreateTensor<float>(memory, cache.data(), cache.size(), cs, 4)};
                auto a =
                    reference.Run(Ort::RunOptions{nullptr}, names, inputs.data(), 2, outputs, 2);
                auto b =
                    candidate.Run(Ort::RunOptions{nullptr}, names, inputs.data(), 2, outputs, 2);
                for (size_t n = 0; n < 2; n++) {
                    size_t size = a[n].GetTensorTypeAndShapeInfo().GetElementCount();
                    if (size != b[n].GetTensorTypeAndShapeInfo().GetElementCount())
                        return 1;
                    const float *p = a[n].GetTensorData<float>(), *q = b[n].GetTensorData<float>();
                    for (size_t i = 0; i < size; i++)
                        if (!std::isfinite(q[i]) ||
                            std::abs(p[i] - q[i]) > 1e-4 + 1e-4 * std::abs(p[i]))
                            return 1;
                }
                std::memcpy(cache.data(), a[1].GetTensorData<float>(),
                            cache.size() * sizeof(float));
                std::printf("BRIDGE_STREAM_EQUIVALENT %d\n", step);
            }
        }
        int status = 0;
        if (waitpid(-1, &status, WNOHANG) != -1 || errno != ECHILD)
            return 1;
        setenv("VOICE_FRONTEND_WORKER", "/bin/false", 1);
        bool rejected = false;
        try {
            Ort::Session invalid(env, argv[2], options);
        } catch (const Ort::Exception &e) {
            rejected = std::strstr(e.what(), "pipe closed") != nullptr;
        }
        if (!rejected || waitpid(-1, &status, WNOHANG) != -1 || errno != ECHILD)
            return 1;
        setenv("VOICE_FRONTEND_WORKER", argv[4], 1);
        setenv("VOICE_TEST_EXIT_ON_REQUEST", "1", 1);
        rejected = false;
        {
            Ort::Session candidate(env, argv[2], options);
            std::array<Ort::Value, 2> inputs = {
                Ort::Value::CreateTensor<float>(memory, x.data(), x.size(), xs, 3),
                Ort::Value::CreateTensor<float>(memory, cache.data(), cache.size(), cs, 4)};
            try {
                candidate.Run(Ort::RunOptions{nullptr}, names, inputs.data(), 2, outputs, 2);
            } catch (const Ort::Exception &e) {
                rejected = std::strstr(e.what(), "pipe closed") != nullptr;
            }
            // Failure must release the worker before Session destruction; the
            // C recognizer cannot depend on catching the resulting ORT exception.
            if (!rejected || waitpid(-1, &status, WNOHANG) != -1 || errno != ECHILD)
                return 1;
        }
        std::puts("BRIDGE_LIFECYCLE_PASSED");
        return 0;
    } catch (const std::exception &e) {
        std::fprintf(stderr, "%s\n", e.what());
        return 1;
    }
}
