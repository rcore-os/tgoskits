#include <onnxruntime_cxx_api.h>

#include <array>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <memory>
#include <stdexcept>
#include <string_view>
#include <vector>

namespace {

using Clock = std::chrono::steady_clock;

constexpr std::size_t kInputCount = 4;
constexpr std::size_t kIdentityCapacity = 64;
constexpr std::string_view kExpectedRuntimeVersion = "1.25.0";
constexpr std::string_view kProvider = "CPUExecutionProvider";

enum IvcOrtStage : std::int32_t {
    kValidateArguments = 1,
    kValidateRuntimeVersion = 2,
    kAllocateContext = 3,
    kCreateEnvironment = 4,
    kConfigureSession = 5,
    kCreateSession = 6,
    kValidateTensorContract = 7,
    kCreateMemoryInfo = 8,
    kCreateInputTensor = 9,
    kRun = 10,
    kValidateOutput = 11,
    kMonotonicClock = 12,
    kDestroyContext = 13,
};

struct IvcOrtStatus {
    std::int32_t stage;
    std::int32_t runtime_status;
};

struct IvcOrtInfo {
    char runtime_version[kIdentityCapacity];
    char provider[kIdentityCapacity];
    std::uint64_t init_us;
};

struct IvcOrtInference {
    float output;
    std::uint64_t wall_ns;
};

struct IvcOrtContext {
    std::unique_ptr<Ort::Env> environment;
    std::unique_ptr<Ort::Session> session;
    std::unique_ptr<Ort::MemoryInfo> memory;
};

void clear_status(IvcOrtStatus *status) {
    if (status != nullptr) {
        status->stage = 0;
        status->runtime_status = 0;
    }
}

int fail(IvcOrtStatus *status, IvcOrtStage stage, std::int32_t runtime_status) {
    if (status != nullptr) {
        status->stage = stage;
        status->runtime_status = runtime_status;
    }
    return -1;
}

void copy_identity(char *destination, std::string_view source) {
    std::memset(destination, 0, kIdentityCapacity);
    std::memcpy(destination, source.data(), source.size());
}

Ort::SessionOptions make_session_options() {
    Ort::SessionOptions options;
    options.SetExecutionMode(ExecutionMode::ORT_SEQUENTIAL);
    options.SetIntraOpNumThreads(1);
    options.SetInterOpNumThreads(1);
    options.DisableMemPattern();
    options.SetGraphOptimizationLevel(GraphOptimizationLevel::ORT_DISABLE_ALL);
    options.AddConfigEntry("session.intra_op.allow_spinning", "0");
    options.AddConfigEntry("session.inter_op.allow_spinning", "0");
    return options;
}

void validate_model_contract(Ort::Session &session) {
    if (session.GetInputCount() != 1 || session.GetOutputCount() != 1) {
        throw std::runtime_error("ORT model must have one input and one output");
    }
    Ort::AllocatorWithDefaultOptions allocator;
    const auto input_name = session.GetInputNameAllocated(0, allocator);
    const auto output_name = session.GetOutputNameAllocated(0, allocator);
    if (std::string_view(input_name.get()) != "normalized_observation" ||
        std::string_view(output_name.get()) != "control_fraction") {
        throw std::runtime_error("ORT model tensor names differ");
    }
    // TensorTypeAndShapeInfo borrows metadata owned by TypeInfo, so keep both
    // owners alive until every shape and element-type check has completed.
    const auto input_type = session.GetInputTypeInfo(0);
    const auto output_type = session.GetOutputTypeInfo(0);
    const auto input_info = input_type.GetTensorTypeAndShapeInfo();
    const auto output_info = output_type.GetTensorTypeAndShapeInfo();
    if (input_info.GetElementType() != ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT ||
        input_info.GetShape() != std::vector<std::int64_t>({1, 4})) {
        throw std::runtime_error("ORT model input contract differs");
    }
    if (output_info.GetElementType() != ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT ||
        output_info.GetShape() != std::vector<std::int64_t>({1, 1})) {
        throw std::runtime_error("ORT model output contract differs");
    }
}

template <typename Operation>
int guard_bridge_call(IvcOrtStatus *status, IvcOrtStage &stage, Operation operation) {
    try {
        return operation();
    } catch (const Ort::Exception &exception) {
        return fail(status, stage, static_cast<std::int32_t>(exception.GetOrtErrorCode()));
    } catch (const std::exception &) {
        return fail(status, stage, -1);
    } catch (...) {
        return fail(status, stage, -2);
    }
}

}  // namespace

extern "C" int ivc_ort_create(const char *model_path, void **raw_context,
                              IvcOrtInfo *info, IvcOrtStatus *status) {
    clear_status(status);
    IvcOrtStage stage = kValidateArguments;
    return guard_bridge_call(status, stage, [&]() {
        if (model_path == nullptr || model_path[0] == '\0' || raw_context == nullptr ||
            info == nullptr) {
            return fail(status, stage, 0);
        }
        *raw_context = nullptr;
        std::memset(info, 0, sizeof(*info));

        stage = kValidateRuntimeVersion;
        const char *runtime_version = OrtGetApiBase()->GetVersionString();
        if (runtime_version == nullptr || runtime_version != kExpectedRuntimeVersion) {
            return fail(status, stage, 0);
        }

        stage = kAllocateContext;
        auto context = std::make_unique<IvcOrtContext>();
        stage = kCreateEnvironment;
        context->environment =
            std::make_unique<Ort::Env>(ORT_LOGGING_LEVEL_WARNING, "ivc-thermal-control");
        stage = kConfigureSession;
        auto options = make_session_options();
        stage = kCreateSession;
        const auto initialization_started = Clock::now();
        context->session =
            std::make_unique<Ort::Session>(*context->environment, model_path, options);
        stage = kValidateTensorContract;
        validate_model_contract(*context->session);
        stage = kCreateMemoryInfo;
        context->memory = std::make_unique<Ort::MemoryInfo>(
            Ort::MemoryInfo::CreateCpu(OrtArenaAllocator, OrtMemTypeDefault));
        const auto initialization_elapsed =
            std::chrono::duration_cast<std::chrono::microseconds>(Clock::now() -
                                                                  initialization_started)
                .count();
        if (initialization_elapsed <= 0) {
            stage = kMonotonicClock;
            return fail(status, stage, 0);
        }

        copy_identity(info->runtime_version, kExpectedRuntimeVersion);
        copy_identity(info->provider, kProvider);
        info->init_us = static_cast<std::uint64_t>(initialization_elapsed);
        *raw_context = context.release();
        return 0;
    });
}

extern "C" int ivc_ort_infer(void *raw_context, const float *inputs,
                             IvcOrtInference *inference, IvcOrtStatus *status) {
    clear_status(status);
    IvcOrtStage stage = kValidateArguments;
    return guard_bridge_call(status, stage, [&]() {
        auto *context = static_cast<IvcOrtContext *>(raw_context);
        if (context == nullptr || context->session == nullptr || context->memory == nullptr ||
            inputs == nullptr || inference == nullptr) {
            return fail(status, stage, 0);
        }
        std::memset(inference, 0, sizeof(*inference));
        std::array<float, kInputCount> input{};
        for (std::size_t index = 0; index < input.size(); ++index) {
            if (!std::isfinite(inputs[index])) {
                return fail(status, stage, 0);
            }
            input[index] = inputs[index];
        }

        stage = kCreateInputTensor;
        constexpr std::array<std::int64_t, 2> kInputShape{1, 4};
        Ort::Value input_tensor = Ort::Value::CreateTensor<float>(
            *context->memory,
            input.data(),
            input.size(),
            kInputShape.data(),
            kInputShape.size());
        constexpr std::array<const char *, 1> kInputNames{"normalized_observation"};
        constexpr std::array<const char *, 1> kOutputNames{"control_fraction"};

        stage = kRun;
        const auto started = Clock::now();
        auto outputs = context->session->Run(
            Ort::RunOptions{nullptr},
            kInputNames.data(),
            &input_tensor,
            1,
            kOutputNames.data(),
            1);
        const auto wall_ns =
            std::chrono::duration_cast<std::chrono::nanoseconds>(Clock::now() - started).count();
        if (wall_ns <= 0) {
            stage = kMonotonicClock;
            return fail(status, stage, 0);
        }

        stage = kValidateOutput;
        if (outputs.size() != 1 || !outputs[0].IsTensor()) {
            return fail(status, stage, 0);
        }
        const auto output_info = outputs[0].GetTensorTypeAndShapeInfo();
        if (output_info.GetElementCount() != 1 ||
            output_info.GetElementType() != ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT) {
            return fail(status, stage, 0);
        }
        const float output = outputs[0].GetTensorData<float>()[0];
        if (!std::isfinite(output)) {
            return fail(status, stage, 0);
        }
        inference->output = output;
        inference->wall_ns = static_cast<std::uint64_t>(wall_ns);
        return 0;
    });
}

extern "C" int ivc_ort_destroy(void *raw_context, IvcOrtStatus *status) {
    clear_status(status);
    IvcOrtStage stage = kDestroyContext;
    return guard_bridge_call(status, stage, [&]() {
        auto *context = static_cast<IvcOrtContext *>(raw_context);
        if (context == nullptr) {
            return fail(status, stage, 0);
        }
        delete context;
        return 0;
    });
}
