#include <onnxruntime_cxx_api.h>

#include <algorithm>
#include <array>
#include <charconv>
#include <chrono>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <fstream>
#include <iomanip>
#include <iostream>
#include <limits>
#include <stdexcept>
#include <string>
#include <string_view>
#include <vector>

namespace {

using Clock = std::chrono::steady_clock;

constexpr std::size_t kInputCount = 4;
constexpr std::size_t kExpectedVectors = 10'000;
constexpr std::size_t kDefaultWarmup = 32;
constexpr std::size_t kDefaultLifecycleCycles = 5;
constexpr double kMaximumF32Error = 1.0e-6;
constexpr double kRoundingBoundaryTolerance = 1.0e-6;
constexpr std::string_view kExpectedRuntimeVersion = "1.25.0";
constexpr std::string_view kCorpusHeader =
    "index,input0_f32_bits,input1_f32_bits,input2_f32_bits,input3_f32_bits,"
    "expected_output_f32_bits,expected_actuator_permille";
constexpr std::string_view kOutputHeader =
    "index,input0_f32_bits,input1_f32_bits,input2_f32_bits,input3_f32_bits,"
    "expected_output_f32_bits,expected_actuator_permille,ort_output_f32_bits,"
    "ort_actuator_permille,wall_ns";

struct Options {
    std::string model_path;
    std::string corpus_path;
    std::string output_path;
    std::string resource_output_path;
    std::size_t warmup = kDefaultWarmup;
    std::size_t lifecycle_cycles = kDefaultLifecycleCycles;
};

struct CorpusRecord {
    std::size_t index = 0;
    std::array<std::uint32_t, kInputCount> input_bits{};
    std::uint32_t expected_output_bits = 0;
    std::uint16_t expected_actuator_permille = 0;
};

struct InferenceResult {
    float output = 0.0F;
    std::uint64_t wall_ns = 0;
};

struct ProcessMemory {
    std::uint64_t rss_kib = 0;
    std::uint64_t peak_rss_kib = 0;
};

struct LifecycleEvidence {
    ProcessMemory before{};
    ProcessMemory first_after_destroy{};
    ProcessMemory final_after_destroy{};
};

[[noreturn]] void fail(const std::string &message) {
    throw std::runtime_error(message);
}

template <typename Integer>
Integer parse_integer(std::string_view value, int base, std::string_view field) {
    Integer result{};
    const auto parsed = std::from_chars(value.data(), value.data() + value.size(), result, base);
    if (parsed.ec != std::errc() || parsed.ptr != value.data() + value.size()) {
        fail("invalid " + std::string(field));
    }
    return result;
}

std::vector<std::string_view> split(std::string_view value, char delimiter) {
    std::vector<std::string_view> fields;
    while (true) {
        const std::size_t offset = value.find(delimiter);
        fields.push_back(value.substr(0, offset));
        if (offset == std::string_view::npos) {
            break;
        }
        value.remove_prefix(offset + 1);
    }
    return fields;
}

std::uint32_t parse_f32_bits(std::string_view value, std::string_view field) {
    if (value.size() != 8) {
        fail("invalid width for " + std::string(field));
    }
    return parse_integer<std::uint32_t>(value, 16, field);
}

float float_from_bits(std::uint32_t bits) {
    float value = 0.0F;
    static_assert(sizeof(value) == sizeof(bits));
    std::memcpy(&value, &bits, sizeof(value));
    return value;
}

std::uint32_t float_bits(float value) {
    std::uint32_t bits = 0;
    static_assert(sizeof(value) == sizeof(bits));
    std::memcpy(&bits, &value, sizeof(bits));
    return bits;
}

std::uint16_t actuator_command(float output) {
    if (!std::isfinite(output) || output < 0.0F || output > 1.0F) {
        fail("model output is outside [0,1]");
    }
    return static_cast<std::uint16_t>(output * 1000.0F + 0.5F);
}

void write_hex32(std::ostream &output, std::uint32_t value) {
    const auto flags = output.flags();
    const char fill = output.fill();
    output << std::hex << std::nouppercase << std::setfill('0') << std::setw(8) << value;
    output.flags(flags);
    output.fill(fill);
}

std::vector<CorpusRecord> read_corpus(const std::string &path) {
    std::ifstream input(path);
    if (!input) {
        fail("cannot open corpus: " + path);
    }
    std::string line;
    if (!std::getline(input, line) || line != kCorpusHeader) {
        fail("corpus header does not match schema 1");
    }
    std::vector<CorpusRecord> records;
    while (std::getline(input, line)) {
        if (line.empty()) {
            fail("corpus contains an empty row");
        }
        const auto fields = split(line, ',');
        if (fields.size() != 7) {
            fail("corpus row has an unexpected field count");
        }
        CorpusRecord record;
        record.index = parse_integer<std::size_t>(fields[0], 10, "index");
        if (record.index != records.size()) {
            fail("corpus indices are not contiguous from zero");
        }
        for (std::size_t index = 0; index < kInputCount; ++index) {
            record.input_bits[index] = parse_f32_bits(fields[index + 1], "input bits");
            if (!std::isfinite(float_from_bits(record.input_bits[index]))) {
                fail("corpus contains a non-finite input");
            }
        }
        record.expected_output_bits = parse_f32_bits(fields[5], "expected output bits");
        const float expected_output = float_from_bits(record.expected_output_bits);
        if (!std::isfinite(expected_output) || expected_output < 0.0F || expected_output > 1.0F) {
            fail("corpus contains an invalid expected output");
        }
        const auto command = parse_integer<std::uint32_t>(fields[6], 10, "expected command");
        if (command > 1000U || actuator_command(expected_output) != command) {
            fail("corpus expected command violates the rounding contract");
        }
        record.expected_actuator_permille = static_cast<std::uint16_t>(command);
        records.push_back(record);
    }
    if (records.size() != kExpectedVectors) {
        fail("corpus must contain exactly 10000 vectors");
    }
    return records;
}

std::uint64_t parse_status_kib(std::string_view line, std::string_view field) {
    line.remove_prefix(field.size());
    const std::size_t value_start = line.find_first_not_of(" \t");
    if (value_start == std::string_view::npos) {
        fail("missing value for " + std::string(field));
    }
    line.remove_prefix(value_start);
    const std::size_t value_end = line.find_first_of(" \t");
    if (value_end == std::string_view::npos) {
        fail("missing unit for " + std::string(field));
    }
    const std::uint64_t value =
        parse_integer<std::uint64_t>(line.substr(0, value_end), 10, field);
    line.remove_prefix(value_end);
    const std::size_t unit_start = line.find_first_not_of(" \t");
    if (unit_start == std::string_view::npos || line.substr(unit_start) != "kB") {
        fail("unexpected unit for " + std::string(field));
    }
    return value;
}

ProcessMemory read_process_memory() {
    std::ifstream status("/proc/self/status");
    if (!status) {
        fail("cannot read /proc/self/status");
    }
    constexpr std::string_view kRssField = "VmRSS:";
    constexpr std::string_view kPeakField = "VmHWM:";
    ProcessMemory memory;
    bool found_rss = false;
    bool found_peak = false;
    std::string line;
    while (std::getline(status, line)) {
        if (line.compare(0, kRssField.size(), kRssField) == 0) {
            memory.rss_kib = parse_status_kib(line, kRssField);
            found_rss = true;
        } else if (line.compare(0, kPeakField.size(), kPeakField) == 0) {
            memory.peak_rss_kib = parse_status_kib(line, kPeakField);
            found_peak = true;
        }
    }
    if (!found_rss || !found_peak || memory.peak_rss_kib < memory.rss_kib) {
        fail("/proc/self/status memory fields are incomplete");
    }
    return memory;
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

Ort::Session create_session(Ort::Env &environment, const std::string &model_path) {
    auto options = make_session_options();
    return Ort::Session(environment, model_path.c_str(), options);
}

void validate_model_contract(Ort::Session &session) {
    if (session.GetInputCount() != 1 || session.GetOutputCount() != 1) {
        fail("ORT model must have exactly one input and one output");
    }
    Ort::AllocatorWithDefaultOptions allocator;
    const auto input_name = session.GetInputNameAllocated(0, allocator);
    const auto output_name = session.GetOutputNameAllocated(0, allocator);
    if (std::string_view(input_name.get()) != "normalized_observation" ||
        std::string_view(output_name.get()) != "control_fraction") {
        fail("ORT model tensor names differ");
    }
    const auto input_type = session.GetInputTypeInfo(0);
    const auto output_type = session.GetOutputTypeInfo(0);
    const auto input_info = input_type.GetTensorTypeAndShapeInfo();
    const auto output_info = output_type.GetTensorTypeAndShapeInfo();
    if (input_info.GetElementType() != ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT ||
        input_info.GetShape() != std::vector<std::int64_t>({1, 4})) {
        fail("ORT model input shape or type differs");
    }
    if (output_info.GetElementType() != ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT ||
        output_info.GetShape() != std::vector<std::int64_t>({1, 1})) {
        fail("ORT model output shape or type differs");
    }
}

InferenceResult infer_one(Ort::Session &session, const CorpusRecord &record) {
    std::array<float, kInputCount> input{};
    for (std::size_t index = 0; index < input.size(); ++index) {
        input[index] = float_from_bits(record.input_bits[index]);
    }
    constexpr std::array<std::int64_t, 2> kInputShape{1, 4};
    const Ort::MemoryInfo memory =
        Ort::MemoryInfo::CreateCpu(OrtArenaAllocator, OrtMemTypeDefault);
    Ort::Value input_tensor = Ort::Value::CreateTensor<float>(
        memory,
        input.data(),
        input.size(),
        kInputShape.data(),
        kInputShape.size());
    constexpr std::array<const char *, 1> kInputNames{"normalized_observation"};
    constexpr std::array<const char *, 1> kOutputNames{"control_fraction"};
    const auto started = Clock::now();
    auto outputs = session.Run(
        Ort::RunOptions{nullptr},
        kInputNames.data(),
        &input_tensor,
        1,
        kOutputNames.data(),
        1);
    const auto elapsed = Clock::now() - started;
    if (outputs.size() != 1 || !outputs[0].IsTensor()) {
        fail("ORT inference returned an invalid output collection");
    }
    const auto output_info = outputs[0].GetTensorTypeAndShapeInfo();
    if (output_info.GetElementCount() != 1 ||
        output_info.GetElementType() != ONNX_TENSOR_ELEMENT_DATA_TYPE_FLOAT) {
        fail("ORT inference returned an invalid output tensor");
    }
    const float output = outputs[0].GetTensorData<float>()[0];
    const auto wall_ns =
        std::chrono::duration_cast<std::chrono::nanoseconds>(elapsed).count();
    if (!std::isfinite(output) || wall_ns <= 0) {
        fail("ORT inference returned a non-finite output or non-positive duration");
    }
    return {output, static_cast<std::uint64_t>(wall_ns)};
}

LifecycleEvidence exercise_lifecycle(
    Ort::Env &environment,
    const Options &options,
    const CorpusRecord &probe) {
    LifecycleEvidence evidence;
    evidence.before = read_process_memory();
    for (std::size_t cycle = 0; cycle < options.lifecycle_cycles; ++cycle) {
        {
            auto session = create_session(environment, options.model_path);
            validate_model_contract(session);
            static_cast<void>(infer_one(session, probe));
        }
        const ProcessMemory after_destroy = read_process_memory();
        if (cycle == 0) {
            evidence.first_after_destroy = after_destroy;
        }
        evidence.final_after_destroy = after_destroy;
        std::cout << "IVC_ORT_LIFECYCLE cycle=" << cycle + 1
                  << " rss_kib=" << after_destroy.rss_kib
                  << " peak_rss_kib=" << after_destroy.peak_rss_kib << '\n';
    }
    return evidence;
}

std::uint64_t percentile(std::vector<std::uint64_t> values, std::size_t percentage) {
    if (values.empty() || percentage > 100) {
        fail("invalid percentile input");
    }
    std::sort(values.begin(), values.end());
    const std::size_t index = ((values.size() - 1) * percentage + 99) / 100;
    return values[index];
}

Options parse_options(int argc, char **argv) {
    Options options;
    for (int index = 1; index < argc; ++index) {
        const std::string_view argument(argv[index]);
        if (argument == "--help") {
            std::cout << "Usage: " << argv[0]
                      << " --model PATH --corpus PATH --output PATH"
                         " --resource-output PATH [--warmup N]"
                         " [--lifecycle-cycles N]\n";
            std::exit(0);
        }
        if (index + 1 >= argc) {
            fail("missing value for " + std::string(argument));
        }
        const std::string value(argv[++index]);
        if (argument == "--model") {
            options.model_path = value;
        } else if (argument == "--corpus") {
            options.corpus_path = value;
        } else if (argument == "--output") {
            options.output_path = value;
        } else if (argument == "--resource-output") {
            options.resource_output_path = value;
        } else if (argument == "--warmup") {
            options.warmup = parse_integer<std::size_t>(value, 10, "warmup");
            if (options.warmup > kExpectedVectors) {
                fail("warmup exceeds 10000");
            }
        } else if (argument == "--lifecycle-cycles") {
            options.lifecycle_cycles =
                parse_integer<std::size_t>(value, 10, "lifecycle cycles");
            if (options.lifecycle_cycles == 0 || options.lifecycle_cycles > 100) {
                fail("lifecycle cycles must be in [1,100]");
            }
        } else {
            fail("unknown option: " + std::string(argument));
        }
    }
    if (options.model_path.empty() || options.corpus_path.empty() ||
        options.output_path.empty() || options.resource_output_path.empty()) {
        fail("model, corpus, output, and resource output are required");
    }
    return options;
}

std::string hex_encode(std::string_view value) {
    constexpr char kHex[] = "0123456789abcdef";
    std::string encoded;
    encoded.reserve(value.size() * 2);
    for (const unsigned char byte : value) {
        encoded.push_back(kHex[byte >> 4]);
        encoded.push_back(kHex[byte & 0x0f]);
    }
    return encoded;
}

int run(const Options &options) {
    const char *runtime_version = OrtGetApiBase()->GetVersionString();
    if (runtime_version == nullptr || runtime_version != kExpectedRuntimeVersion) {
        fail("ONNX Runtime version differs from 1.25.0");
    }
    const auto corpus = read_corpus(options.corpus_path);
    Ort::Env environment(ORT_LOGGING_LEVEL_WARNING, "ivc-thermal-ort");
    const LifecycleEvidence lifecycle =
        exercise_lifecycle(environment, options, corpus.front());

    const auto initialization_started = Clock::now();
    auto session = create_session(environment, options.model_path);
    validate_model_contract(session);
    const auto initialization_us = std::chrono::duration_cast<std::chrono::microseconds>(
                                       Clock::now() - initialization_started)
                                       .count();
    if (initialization_us <= 0) {
        fail("ORT session initialization duration is not positive");
    }
    for (std::size_t index = 0; index < options.warmup; ++index) {
        static_cast<void>(infer_one(session, corpus[index % corpus.size()]));
    }

    std::ofstream output(options.output_path, std::ios::trunc);
    if (!output) {
        fail("cannot create output CSV: " + options.output_path);
    }
    output << kOutputHeader << '\n';
    double maximum_error = 0.0;
    std::size_t exact_commands = 0;
    std::size_t rounding_boundary_equivalences = 0;
    std::size_t material_command_mismatches = 0;
    std::vector<std::uint64_t> wall_times;
    wall_times.reserve(corpus.size());
    for (const auto &record : corpus) {
        const InferenceResult result = infer_one(session, record);
        wall_times.push_back(result.wall_ns);
        const float expected_output = float_from_bits(record.expected_output_bits);
        maximum_error = std::max(
            maximum_error,
            std::abs(static_cast<double>(result.output) - expected_output));
        const std::uint16_t command = actuator_command(result.output);
        const int command_delta =
            static_cast<int>(command) - static_cast<int>(record.expected_actuator_permille);
        if (command_delta == 0) {
            ++exact_commands;
        } else {
            const int lower_command =
                std::min<int>(command, record.expected_actuator_permille);
            const double boundary = (static_cast<double>(lower_command) + 0.5) / 1000.0;
            if (std::abs(command_delta) == 1 &&
                std::abs(static_cast<double>(result.output) - boundary) <=
                    kRoundingBoundaryTolerance &&
                std::abs(static_cast<double>(expected_output) - boundary) <=
                    kRoundingBoundaryTolerance) {
                ++rounding_boundary_equivalences;
            } else {
                ++material_command_mismatches;
            }
        }
        output << record.index;
        for (const std::uint32_t bits : record.input_bits) {
            output << ',';
            write_hex32(output, bits);
        }
        output << ',';
        write_hex32(output, record.expected_output_bits);
        output << ',' << record.expected_actuator_permille << ',';
        write_hex32(output, float_bits(result.output));
        output << ',' << command << ',' << result.wall_ns << '\n';
        if ((record.index + 1) % 1000 == 0) {
            std::cout << "IVC_ORT_PROGRESS completed=" << record.index + 1 << '\n';
        }
    }
    output.flush();
    if (!output) {
        fail("cannot flush ORT output CSV");
    }
    if (maximum_error > kMaximumF32Error || material_command_mismatches != 0) {
        fail("ORT numerical gate failed");
    }

    const std::uint64_t wall_p50_ns = percentile(wall_times, 50);
    const std::uint64_t wall_p95_ns = percentile(wall_times, 95);
    const std::uint64_t wall_p99_ns = percentile(wall_times, 99);
    const std::uint64_t wall_max_ns = *std::max_element(wall_times.begin(), wall_times.end());
    session = Ort::Session{nullptr};
    const ProcessMemory after_main_destroy = read_process_memory();

    std::ofstream resource(options.resource_output_path, std::ios::trunc);
    if (!resource) {
        fail("cannot create resource output: " + options.resource_output_path);
    }
    resource << "schema=1\n"
             << "backend=onnxruntime-cpu\n"
             << "runtime_version=" << runtime_version << '\n'
             << "lifecycle_cycles=" << options.lifecycle_cycles << '\n'
             << "rss_before_kib=" << lifecycle.before.rss_kib << '\n'
             << "rss_first_after_destroy_kib=" << lifecycle.first_after_destroy.rss_kib << '\n'
             << "rss_lifecycle_final_kib=" << lifecycle.final_after_destroy.rss_kib << '\n'
             << "rss_after_main_destroy_kib=" << after_main_destroy.rss_kib << '\n'
             << "peak_rss_kib=" << after_main_destroy.peak_rss_kib << '\n'
             << "initialization_us=" << initialization_us << '\n'
             << "wall_p50_ns=" << wall_p50_ns << '\n'
             << "wall_p95_ns=" << wall_p95_ns << '\n'
             << "wall_p99_ns=" << wall_p99_ns << '\n'
             << "wall_max_ns=" << wall_max_ns << '\n'
             << "maximum_absolute_error=" << std::setprecision(17) << maximum_error << '\n'
             << "exact_actuator_matches=" << exact_commands << '\n'
             << "rounding_boundary_equivalences=" << rounding_boundary_equivalences << '\n'
             << "material_actuator_mismatches=" << material_command_mismatches << '\n';
    resource.flush();
    if (!resource) {
        fail("cannot flush ORT resource output");
    }

    std::cout << "IVC_ORT_RUNTIME version=" << runtime_version
              << " backend=onnxruntime-cpu provider=CPUExecutionProvider"
              << " init_us=" << initialization_us << '\n';
    std::cout << std::setprecision(17)
              << "IVC_ORT_RESULT vectors=" << corpus.size()
              << " max_abs_error=" << maximum_error
              << " exact_commands=" << exact_commands
              << " rounding_equivalences=" << rounding_boundary_equivalences
              << " material_mismatches=" << material_command_mismatches
              << " wall_p99_ns=" << wall_p99_ns
              << " wall_max_ns=" << wall_max_ns << '\n';
    return 0;
}

}  // namespace

int main(int argc, char **argv) {
    try {
        return run(parse_options(argc, argv));
    } catch (const std::exception &error) {
        std::cerr << "IVC_ORT_ERROR message_hex=" << hex_encode(error.what()) << '\n';
        return 1;
    }
}
