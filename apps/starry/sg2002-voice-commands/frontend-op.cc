// SPDX-License-Identifier: Apache-2.0
#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#define ORT_API_MANUAL_INIT
#include "frontend-protocol.h"
#include <algorithm>
#include <cerrno>
#include <chrono>
#include <csignal>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <fcntl.h>
#include <onnxruntime_cxx_api.h>
#include <poll.h>
#include <stdexcept>
#include <sys/wait.h>
#include <unistd.h>

namespace
{
void close_fd(int &fd)
{
    if (fd >= 0)
        close(fd);
    fd = -1;
}
// One ORT kernel owns one musl worker. Only pipes cross the libc boundary.
// The application ignores SIGPIPE so a worker failure becomes an ORT error.
class Frontend
{
    int write_fd = -1, read_fd = -1;
    pid_t child = -1;
    void transfer(bool writing, void *data, size_t bytes)
    {
        const auto end = std::chrono::steady_clock::now() + std::chrono::seconds(30);
        auto *p = static_cast<unsigned char *>(data);
        while (bytes) {
            auto remaining = std::chrono::duration_cast<std::chrono::milliseconds>(
                                 end - std::chrono::steady_clock::now())
                                 .count();
            if (remaining <= 0)
                throw std::runtime_error("TPU frontend timed out");
            pollfd f{writing ? write_fd : read_fd, static_cast<short>(writing ? POLLOUT : POLLIN),
                     0};
            int ready = poll(&f, 1, static_cast<int>(std::min<int64_t>(1000, remaining)));
            if (ready < 0 && errno == EINTR)
                continue;
            if (ready < 0)
                throw std::runtime_error("TPU frontend polling failed");
            if (!ready)
                continue;
            ssize_t n = writing ? write(f.fd, p, bytes) : read(f.fd, p, bytes);
            if (n < 0 && (errno == EAGAIN || errno == EINTR))
                continue;
            if (n <= 0)
                throw std::runtime_error("TPU frontend pipe closed or failed");
            p += n;
            bytes -= static_cast<size_t>(n);
        }
    }
    void stop() noexcept
    {
        close_fd(write_fd);
        close_fd(read_fd);
        if (child <= 0)
            return;
        for (int i = 0; i < 100; i++) {
            int status;
            pid_t got = waitpid(child, &status, WNOHANG);
            if (got == child || (got < 0 && errno == ECHILD)) {
                child = -1;
                return;
            }
            struct timespec delay {
                0, 20000000
            };
            nanosleep(&delay, nullptr);
        }
        kill(child, SIGKILL);
        while (waitpid(child, nullptr, 0) < 0 && errno == EINTR) {
        }
        child = -1;
    }

  public:
    Frontend()
    {
        const char *worker = std::getenv("VOICE_FRONTEND_WORKER");
        const char *model = std::getenv("VOICE_FRONTEND_MODEL");
        if (!worker || !model || worker[0] != '/' || model[0] != '/')
            throw std::runtime_error("absolute TPU worker/model paths are required");
        int request[2] = {-1, -1}, response[2] = {-1, -1};
        if (pipe2(request, O_CLOEXEC) || pipe2(response, O_CLOEXEC)) {
            for (int &fd : request)
                close_fd(fd);
            for (int &fd : response)
                close_fd(fd);
            throw std::runtime_error("cannot create TPU frontend pipes");
        }
        child = fork();
        if (child == 0) {
            if (dup2(request[0], STDIN_FILENO) < 0 || dup2(response[1], STDOUT_FILENO) < 0)
                _exit(126);
            for (int fd : request)
                close(fd);
            for (int fd : response)
                close(fd);
            execl(worker, worker, model, static_cast<char *>(nullptr));
            _exit(127);
        }
        close_fd(request[0]);
        close_fd(response[1]);
        write_fd = request[1];
        read_fd = response[0];
        try {
            if (child < 0)
                throw std::runtime_error("cannot fork TPU frontend");
            if (fcntl(write_fd, F_SETFL, O_NONBLOCK) < 0 || fcntl(read_fd, F_SETFL, O_NONBLOCK) < 0)
                throw std::runtime_error("cannot configure TPU pipes");
            uint32_t actual[sizeof(voice_frontend_header) / sizeof(uint32_t)];
            transfer(false, actual, sizeof(actual));
            if (memcmp(actual, voice_frontend_header, sizeof(actual)))
                throw std::runtime_error("TPU frontend protocol mismatch");
        } catch (...) {
            stop();
            throw;
        }
    }
    ~Frontend()
    {
        stop();
    }
    Frontend(const Frontend &) = delete;
    Frontend &operator=(const Frontend &) = delete;
    void run(const float *x, const float *cache, float *features, float *next)
    {
        try {
            transfer(true, const_cast<float *>(x), VOICE_MEL_COUNT * sizeof(float));
            transfer(true, const_cast<float *>(cache), VOICE_CACHE_COUNT * sizeof(float));
            transfer(false, features, VOICE_FEATURE_COUNT * sizeof(float));
            transfer(false, next, VOICE_CACHE_COUNT * sizeof(float));
        } catch (...) {
            // Reap the worker before publishing failure: Sherpa's C entry point
            // does not catch the ORT exception or guarantee Session destruction.
            stop();
            throw;
        }
    }
};
struct Kernel {
    Frontend frontend;
    OrtStatusPtr ComputeV2(OrtKernelContext *raw) noexcept
    {
        try {
            Ort::KernelContext ctx(raw);
            auto x = ctx.GetInput(0);
            auto cache = ctx.GetInput(1);
            if (x.GetTensorTypeAndShapeInfo().GetShape() != std::vector<int64_t>({1, 45, 80}) ||
                cache.GetTensorTypeAndShapeInfo().GetShape() !=
                    std::vector<int64_t>({1, 128, 3, 19}))
                throw std::runtime_error("TPU frontend only accepts the pinned batch-one shapes");
            const int64_t fshape[] = {1, 16, 128}, cshape[] = {1, 128, 3, 19};
            auto features = ctx.GetOutput(0, fshape, 3);
            auto next = ctx.GetOutput(1, cshape, 4);
            frontend.run(x.GetTensorData<float>(), cache.GetTensorData<float>(),
                         features.GetTensorMutableData<float>(),
                         next.GetTensorMutableData<float>());
            return nullptr;
        } catch (const std::exception &error) {
            return Ort::GetApi().CreateStatus(ORT_FAIL, error.what());
        } catch (...) {
            return Ort::GetApi().CreateStatus(ORT_FAIL, "unknown TPU frontend failure");
        }
    }
};
struct Operation : Ort::CustomOpBase<Operation, Kernel, true> {
    const char *GetName() const
    {
        return "VoiceFrontend";
    }
    const char *GetExecutionProviderType() const
    {
        return "CPUExecutionProvider";
    }
    size_t GetInputTypeCount() const
    {
        return 2;
    }
    size_t GetOutputTypeCount() const
    {
        return 2;
    }
    ONNXTensorElementDataType GetInputType(size_t) const
    {
        return ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT;
    }
    ONNXTensorElementDataType GetOutputType(size_t) const
    {
        return ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT;
    }
    OrtStatusPtr CreateKernelV2(const OrtApi &api, const OrtKernelInfo *,
                                void **result) const noexcept
    {
        *result = nullptr;
        try {
            *result = new Kernel;
            return nullptr;
        } catch (const std::exception &error) {
            return api.CreateStatus(ORT_FAIL, error.what());
        } catch (...) {
            return api.CreateStatus(ORT_FAIL, "cannot initialize TPU frontend");
        }
    }
};
} // namespace
extern "C" OrtStatus *RegisterCustomOps(OrtSessionOptions *options, const OrtApiBase *base)
{
    const OrtApi *api = base->GetApi(ORT_API_VERSION);
    if (!api)
        return base->GetApi(1)->CreateStatus(ORT_FAIL, "ORT 1.21 or newer is required");
    Ort::InitApi(api);
    try {
        static Operation operation;
        static Ort::CustomOpDomain domain = []() {
            Ort::CustomOpDomain d("voice.sg2002");
            d.Add(&operation);
            return d;
        }();
        return api->AddCustomOpDomain(options, domain);
    } catch (const std::exception &error) {
        return api->CreateStatus(ORT_FAIL, error.what());
    } catch (...) {
        return api->CreateStatus(ORT_FAIL, "cannot register TPU frontend");
    }
}
