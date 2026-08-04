#include <rknn_api.h>

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
#include <thread>
#include <vector>

namespace {

constexpr std::size_t kInputCount = 4;
constexpr std::size_t kExpectedVectors = 10'000;
constexpr std::size_t kDefaultWarmup = 32;
constexpr std::size_t kMaximumLifecycleCycles = 100;
constexpr double kMaximumF32Error = 0.002;
constexpr int kMaximumActuatorDelta = 2;
constexpr std::string_view kCorpusHeader =
    "index,input0_f32_bits,input1_f32_bits,input2_f32_bits,input3_f32_bits,"
    "expected_output_f32_bits,expected_actuator_permille";
constexpr std::string_view kOutputHeader =
    "index,input0_f32_bits,input1_f32_bits,input2_f32_bits,input3_f32_bits,"
    "expected_output_f32_bits,expected_actuator_permille,rknn_output_f32_bits,"
    "rknn_actuator_permille,wall_ns,device_us";

struct Options {
    std::string model_path;
    std::string corpus_path;
    std::string output_path;
    std::size_t warmup = kDefaultWarmup;
    rknn_core_mask core_mask = RKNN_NPU_CORE_0;
    std::string core_mask_name = "0";
    std::size_t evidence_marker_copies = 1;
    std::uint64_t evidence_marker_interval_ms = 0;
    std::size_t lifecycle_cycles = 1;
    std::string resource_output_path;
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
    std::int64_t device_us = 0;
};

struct ProcessMemory {
    std::uint64_t rss_kib = 0;
    std::uint64_t peak_rss_kib = 0;
};

struct LifecycleEvidence {
    std::size_t cycles = 0;
    std::size_t probe_inferences = 0;
    std::int64_t cold_init_us = -1;
    ProcessMemory baseline{};
    std::uint64_t first_post_destroy_rss_kib = 0;
    std::uint64_t final_post_destroy_rss_kib = 0;
    std::uint64_t maximum_post_destroy_rss_kib = 0;
    std::uint64_t peak_rss_kib = 0;
    std::int64_t final_post_destroy_growth_kib = 0;
    std::int64_t maximum_post_destroy_growth_kib = 0;
};

class Context {
  public:
    Context() = default;
    Context(const Context &) = delete;
    Context &operator=(const Context &) = delete;

    ~Context() {
        if (value_ != 0) {
            rknn_destroy(value_);
        }
    }

    rknn_context *out() { return &value_; }
    rknn_context get() const { return value_; }

    int destroy() {
        if (value_ == 0) {
            return RKNN_SUCC;
        }
        const rknn_context value = value_;
        value_ = 0;
        return rknn_destroy(value);
    }

  private:
    rknn_context value_ = 0;
};

[[noreturn]] void fail(const std::string &message) { throw std::runtime_error(message); }

void require_status(int status, std::string_view operation) {
    if (status != RKNN_SUCC) {
        fail(std::string(operation) + " failed with status " + std::to_string(status));
    }
}

std::string hex_encode(std::string_view value) {
    static constexpr char kDigits[] = "0123456789abcdef";
    std::string encoded;
    encoded.reserve(value.size() * 2);
    for (unsigned char byte : value) {
        encoded.push_back(kDigits[byte >> 4U]);
        encoded.push_back(kDigits[byte & 0x0fU]);
    }
    return encoded;
}

std::string_view runtime_api_compatibility_identity(std::string_view version) {
    return version.substr(0, version.find_first_of(" \t"));
}

template <typename Writer>
void write_redundant_marker(const Options &options, Writer write_marker) {
    for (std::size_t copy = 0; copy < options.evidence_marker_copies; ++copy) {
        write_marker();
        std::cout << std::endl;
        if (copy + 1 < options.evidence_marker_copies) {
            std::this_thread::sleep_for(
                std::chrono::milliseconds(options.evidence_marker_interval_ms));
        }
    }
}

std::string bounded_string(const char *value, std::size_t capacity) {
    std::size_t length = 0;
    while (length < capacity && value[length] != '\0') {
        ++length;
    }
    return std::string(value, length);
}

std::vector<std::string_view> split(std::string_view line, char separator) {
    std::vector<std::string_view> fields;
    while (true) {
        const std::size_t position = line.find(separator);
        fields.push_back(line.substr(0, position));
        if (position == std::string_view::npos) {
            return fields;
        }
        line.remove_prefix(position + 1);
    }
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

std::uint64_t parse_status_kib(std::string_view line, std::string_view field) {
    line.remove_prefix(field.size());
    const std::size_t value_start = line.find_first_not_of(" \t");
    if (value_start == std::string_view::npos) {
        fail("missing value for " + std::string(field));
    }
    line.remove_prefix(value_start);
    const std::size_t value_end = line.find_first_of(" \t");
    if (value_end == std::string_view::npos) {
        fail("missing kB unit for " + std::string(field));
    }
    const std::uint64_t value =
        parse_integer<std::uint64_t>(line.substr(0, value_end), 10, field);
    line.remove_prefix(value_end);
    const std::size_t unit_start = line.find_first_not_of(" \t");
    if (unit_start == std::string_view::npos || line.substr(unit_start) != "kB") {
        fail("unexpected unit for " + std::string(field));
    }
    if (value == 0) {
        fail("zero value for " + std::string(field));
    }
    return value;
}

ProcessMemory read_process_memory() {
    std::ifstream status("/proc/self/status");
    if (!status) {
        fail("cannot open /proc/self/status");
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
    if (!found_rss || !found_peak) {
        fail("/proc/self/status lacks VmRSS or VmHWM");
    }
    if (memory.peak_rss_kib < memory.rss_kib) {
        fail("VmHWM is below VmRSS");
    }
    return memory;
}

std::int64_t signed_difference(std::uint64_t value, std::uint64_t baseline) {
    constexpr std::uint64_t kMaximumSigned =
        static_cast<std::uint64_t>(std::numeric_limits<std::int64_t>::max());
    if (value >= baseline) {
        const std::uint64_t difference = value - baseline;
        if (difference > kMaximumSigned) {
            fail("positive RSS difference exceeds int64");
        }
        return static_cast<std::int64_t>(difference);
    }
    const std::uint64_t difference = baseline - value;
    if (difference > kMaximumSigned) {
        fail("negative RSS difference exceeds int64");
    }
    return -static_cast<std::int64_t>(difference);
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
    std::memcpy(&bits, &value, sizeof(bits));
    return bits;
}

std::uint16_t actuator_command(float output) {
    if (!std::isfinite(output) || output < 0.0F || output > 1.0F) {
        fail("RKNN output is outside the finite [0,1] contract");
    }
    const float scaled = output * 1000.0F + 0.5F;
    const auto command = static_cast<std::uint32_t>(scaled);
    if (command > 1000U) {
        fail("RKNN actuator command exceeds 1000");
    }
    return static_cast<std::uint16_t>(command);
}

std::vector<std::uint8_t> read_binary(const std::string &path) {
    std::ifstream input(path, std::ios::binary | std::ios::ate);
    if (!input) {
        fail("cannot open model: " + path);
    }
    const std::streamoff end = input.tellg();
    if (end <= 0 || static_cast<std::uint64_t>(end) > std::numeric_limits<std::uint32_t>::max()) {
        fail("model size is invalid");
    }
    std::vector<std::uint8_t> bytes(static_cast<std::size_t>(end));
    input.seekg(0);
    if (!input.read(reinterpret_cast<char *>(bytes.data()), end)) {
        fail("cannot read complete model");
    }
    return bytes;
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
            fail("corpus row has the wrong field count");
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

Options parse_options(int argc, char **argv) {
    Options options;
    for (int index = 1; index < argc; ++index) {
        const std::string_view argument(argv[index]);
        if (argument == "--help") {
            std::cout << "Usage: " << argv[0]
                      << " --model PATH --corpus PATH --output PATH"
                         " [--warmup N] [--core-mask 0|1|2|auto|all]"
                         " [--evidence-marker-copies N]"
                         " [--evidence-marker-interval-ms N]"
                         " [--lifecycle-cycles N] [--resource-output PATH]\n";
            std::exit(0);
        }
        if (index + 1 >= argc) {
            fail("missing value for " + std::string(argument));
        }
        const std::string_view value(argv[++index]);
        if (argument == "--model") {
            options.model_path = value;
        } else if (argument == "--corpus") {
            options.corpus_path = value;
        } else if (argument == "--output") {
            options.output_path = value;
        } else if (argument == "--warmup") {
            options.warmup = parse_integer<std::size_t>(value, 10, "warmup");
            if (options.warmup > 10'000) {
                fail("warmup exceeds 10000");
            }
        } else if (argument == "--evidence-marker-copies") {
            options.evidence_marker_copies =
                parse_integer<std::size_t>(value, 10, "evidence marker copies");
            if (options.evidence_marker_copies == 0 || options.evidence_marker_copies > 5) {
                fail("evidence marker copies must be in [1,5]");
            }
        } else if (argument == "--evidence-marker-interval-ms") {
            options.evidence_marker_interval_ms =
                parse_integer<std::uint64_t>(value, 10, "evidence marker interval");
            if (options.evidence_marker_interval_ms > 1000) {
                fail("evidence marker interval exceeds 1000 ms");
            }
        } else if (argument == "--lifecycle-cycles") {
            options.lifecycle_cycles =
                parse_integer<std::size_t>(value, 10, "lifecycle cycles");
            if (options.lifecycle_cycles == 0 ||
                options.lifecycle_cycles > kMaximumLifecycleCycles) {
                fail("lifecycle cycles must be in [1,100]");
            }
        } else if (argument == "--resource-output") {
            options.resource_output_path = value;
        } else if (argument == "--core-mask") {
            options.core_mask_name = value;
            if (value == "0") {
                options.core_mask = RKNN_NPU_CORE_0;
            } else if (value == "1") {
                options.core_mask = RKNN_NPU_CORE_1;
            } else if (value == "2") {
                options.core_mask = RKNN_NPU_CORE_2;
            } else if (value == "auto") {
                options.core_mask = RKNN_NPU_CORE_AUTO;
            } else if (value == "all") {
                options.core_mask = RKNN_NPU_CORE_ALL;
            } else {
                fail("unsupported core mask");
            }
        } else {
            fail("unknown option: " + std::string(argument));
        }
    }
    if (options.model_path.empty() || options.corpus_path.empty() || options.output_path.empty()) {
        fail("--model, --corpus, and --output are required");
    }
    if (options.lifecycle_cycles > 1 && options.resource_output_path.empty()) {
        fail("--resource-output is required for repeated lifecycle evidence");
    }
    return options;
}

void query_tensor_contract(rknn_context context, rknn_tensor_attr *input_attr,
                           rknn_tensor_attr *output_attr) {
    rknn_input_output_num counts{};
    require_status(rknn_query(context, RKNN_QUERY_IN_OUT_NUM, &counts, sizeof(counts)),
                   "query input/output counts");
    if (counts.n_input != 1 || counts.n_output != 1) {
        fail("model must expose exactly one input and one output");
    }

    input_attr->index = 0;
    output_attr->index = 0;
    require_status(rknn_query(context, RKNN_QUERY_INPUT_ATTR, input_attr, sizeof(*input_attr)),
                   "query input attribute");
    require_status(rknn_query(context, RKNN_QUERY_OUTPUT_ATTR, output_attr, sizeof(*output_attr)),
                   "query output attribute");
}

void validate_tensor_contract(const rknn_tensor_attr &input_attr,
                              const rknn_tensor_attr &output_attr) {
    if (bounded_string(input_attr.name, sizeof(input_attr.name)) != "normalized_observation" ||
        input_attr.n_elems != kInputCount || input_attr.type != RKNN_TENSOR_FLOAT16) {
        fail("compiled input tensor does not match normalized_observation float16[4]");
    }
    if (bounded_string(output_attr.name, sizeof(output_attr.name)) != "control_fraction" ||
        output_attr.n_elems != 1 || output_attr.type != RKNN_TENSOR_FLOAT16) {
        fail("compiled output tensor does not match control_fraction float16[1]");
    }
}

InferenceResult infer_one(rknn_context context, const rknn_tensor_attr &input_attr,
                          const CorpusRecord &record) {
    std::array<float, kInputCount> inputs{};
    std::transform(record.input_bits.begin(), record.input_bits.end(), inputs.begin(), float_from_bits);

    rknn_input input{};
    input.index = 0;
    input.buf = inputs.data();
    input.size = static_cast<std::uint32_t>(sizeof(inputs));
    input.pass_through = 0;
    input.type = RKNN_TENSOR_FLOAT32;
    input.fmt = input_attr.fmt;

    const auto started = std::chrono::steady_clock::now();
    require_status(rknn_inputs_set(context, 1, &input), "set input");
    require_status(rknn_run(context, nullptr), "run inference");

    rknn_output output{};
    output.index = 0;
    output.want_float = 1;
    output.is_prealloc = 0;
    require_status(rknn_outputs_get(context, 1, &output, nullptr), "get output");
    if (output.buf == nullptr || output.size < sizeof(float)) {
        rknn_outputs_release(context, 1, &output);
        fail("runtime returned an invalid float output buffer");
    }
    float value = 0.0F;
    std::memcpy(&value, output.buf, sizeof(value));
    const auto finished = std::chrono::steady_clock::now();

    rknn_perf_run performance{};
    const int performance_status =
        rknn_query(context, RKNN_QUERY_PERF_RUN, &performance, sizeof(performance));
    const int release_status = rknn_outputs_release(context, 1, &output);
    require_status(performance_status, "query device performance");
    require_status(release_status, "release output");
    if (performance.run_duration <= 0) {
        fail("runtime returned a non-positive device duration");
    }

    const auto wall = std::chrono::duration_cast<std::chrono::nanoseconds>(finished - started);
    if (wall.count() <= 0) {
        fail("host inference duration is non-positive");
    }
    return {value, static_cast<std::uint64_t>(wall.count()), performance.run_duration};
}

void load_context(Context &context, const std::vector<std::uint8_t> &model,
                  const Options &options) {
    require_status(rknn_init(context.out(), const_cast<std::uint8_t *>(model.data()),
                             static_cast<std::uint32_t>(model.size()),
                             RKNN_FLAG_COLLECT_PERF_MASK, nullptr),
                   "initialize RKNN context");
    require_status(rknn_set_core_mask(context.get(), options.core_mask),
                   "set NPU core mask");
}

void record_post_destroy_memory(LifecycleEvidence &evidence,
                                const ProcessMemory &memory) {
    if (evidence.first_post_destroy_rss_kib == 0) {
        evidence.first_post_destroy_rss_kib = memory.rss_kib;
    }
    evidence.maximum_post_destroy_rss_kib =
        std::max(evidence.maximum_post_destroy_rss_kib, memory.rss_kib);
    evidence.peak_rss_kib = std::max(evidence.peak_rss_kib, memory.peak_rss_kib);
}

LifecycleEvidence run_lifecycle_probes(const Options &options,
                                       const std::vector<std::uint8_t> &model,
                                       const std::vector<CorpusRecord> &corpus) {
    LifecycleEvidence evidence;
    evidence.cycles = options.lifecycle_cycles;
    evidence.probe_inferences = options.lifecycle_cycles - 1;
    evidence.baseline = read_process_memory();
    evidence.peak_rss_kib = evidence.baseline.peak_rss_kib;

    for (std::size_t cycle = 0; cycle < evidence.probe_inferences; ++cycle) {
        Context probe_context;
        const auto init_started = std::chrono::steady_clock::now();
        load_context(probe_context, model, options);
        const auto init_finished = std::chrono::steady_clock::now();
        if (cycle == 0) {
            evidence.cold_init_us =
                std::chrono::duration_cast<std::chrono::microseconds>(
                    init_finished - init_started)
                    .count();
            if (evidence.cold_init_us <= 0) {
                fail("cold RKNN context initialization duration is not positive");
            }
        }
        rknn_tensor_attr input_attr{};
        rknn_tensor_attr output_attr{};
        query_tensor_contract(probe_context.get(), &input_attr, &output_attr);
        validate_tensor_contract(input_attr, output_attr);
        static_cast<void>(infer_one(probe_context.get(), input_attr,
                                    corpus[cycle % corpus.size()]));
        require_status(probe_context.destroy(), "destroy RKNN probe context");
        record_post_destroy_memory(evidence, read_process_memory());
    }
    return evidence;
}

void finalize_lifecycle_evidence(LifecycleEvidence &evidence) {
    const ProcessMemory final_memory = read_process_memory();
    record_post_destroy_memory(evidence, final_memory);
    evidence.final_post_destroy_rss_kib = final_memory.rss_kib;
    evidence.final_post_destroy_growth_kib = signed_difference(
        evidence.final_post_destroy_rss_kib,
        evidence.first_post_destroy_rss_kib);
    evidence.maximum_post_destroy_growth_kib = signed_difference(
        evidence.maximum_post_destroy_rss_kib,
        evidence.first_post_destroy_rss_kib);
}

void write_resource_evidence(const Options &options,
                             const LifecycleEvidence &evidence) {
    if (options.resource_output_path.empty()) {
        return;
    }
    std::ofstream output(options.resource_output_path,
                         std::ios::binary | std::ios::trunc);
    if (!output) {
        fail("cannot create resource evidence: " + options.resource_output_path);
    }
    output << "schema=1\n"
           << "lifecycle_cycles=" << evidence.cycles << '\n'
           << "probe_inferences=" << evidence.probe_inferences << '\n'
           << "context_init_count=" << evidence.cycles << '\n'
           << "context_destroy_count=" << evidence.cycles << '\n'
           << "baseline_rss_kib=" << evidence.baseline.rss_kib << '\n'
           << "first_post_destroy_rss_kib="
           << evidence.first_post_destroy_rss_kib << '\n'
           << "final_post_destroy_rss_kib="
           << evidence.final_post_destroy_rss_kib << '\n'
           << "maximum_post_destroy_rss_kib="
           << evidence.maximum_post_destroy_rss_kib << '\n'
           << "peak_rss_kib=" << evidence.peak_rss_kib << '\n'
           << "final_post_destroy_growth_kib="
           << evidence.final_post_destroy_growth_kib << '\n'
           << "maximum_post_destroy_growth_kib="
           << evidence.maximum_post_destroy_growth_kib << '\n';
    output.close();
    if (!output) {
        fail("failed while writing resource evidence");
    }
}

void write_resource_markers(const Options &options,
                            const LifecycleEvidence &evidence) {
    write_redundant_marker(options, [&] {
        std::cout << "IVC_RKNN_LIFECYCLE cycles=" << evidence.cycles
                  << " probe_inferences=" << evidence.probe_inferences
                  << " init_count=" << evidence.cycles
                  << " destroy_count=" << evidence.cycles;
    });
    write_redundant_marker(options, [&] {
        std::cout << "IVC_RKNN_MEMORY_BASELINE rss_kib="
                  << evidence.baseline.rss_kib
                  << " first_post_destroy_rss_kib="
                  << evidence.first_post_destroy_rss_kib;
    });
    write_redundant_marker(options, [&] {
        std::cout << "IVC_RKNN_MEMORY_FINAL rss_kib="
                  << evidence.final_post_destroy_rss_kib
                  << " peak_rss_kib=" << evidence.peak_rss_kib
                  << " maximum_post_destroy_growth_kib="
                  << evidence.maximum_post_destroy_growth_kib;
    });
}

void write_hex32(std::ostream &output, std::uint32_t value) {
    output << std::hex << std::setw(8) << std::setfill('0') << value << std::dec;
}

int run(const Options &options) {
    const auto corpus = read_corpus(options.corpus_path);
    const auto model = read_binary(options.model_path);
    std::cout << "IVC_RKNN_LINUX_BEGIN schema=1 vectors=" << corpus.size()
              << " warmup=" << options.warmup << " core_mask=" << options.core_mask_name << '\n';

    LifecycleEvidence lifecycle = run_lifecycle_probes(options, model, corpus);
    Context context;
    const auto init_started = std::chrono::steady_clock::now();
    load_context(context, model, options);
    const auto init_finished = std::chrono::steady_clock::now();

    rknn_sdk_version versions{};
    require_status(rknn_query(context.get(), RKNN_QUERY_SDK_VERSION, &versions, sizeof(versions)),
                   "query SDK version");
    const std::string api_version = bounded_string(versions.api_version, sizeof(versions.api_version));
    const std::string driver_version = bounded_string(versions.drv_version, sizeof(versions.drv_version));
    const std::string_view api_compatibility_identity =
        runtime_api_compatibility_identity(api_version);
    if (options.evidence_marker_copies == 1) {
        write_redundant_marker(options, [&] {
            std::cout << "IVC_RKNN_RUNTIME api_version_hex=" << hex_encode(api_version)
                      << " driver_version_hex=" << hex_encode(driver_version);
        });
    } else {
        write_redundant_marker(options, [&] {
            std::cout << "IVC_RKNN_RUNTIME_API version_hex="
                      << hex_encode(api_compatibility_identity);
        });
        write_redundant_marker(options, [&] {
            std::cout << "IVC_RKNN_RUNTIME_DRIVER version_hex=" << hex_encode(driver_version);
        });
    }

    rknn_tensor_attr input_attr{};
    rknn_tensor_attr output_attr{};
    query_tensor_contract(context.get(), &input_attr, &output_attr);
    std::cout << "IVC_RKNN_TENSOR input_name_hex="
              << hex_encode(bounded_string(input_attr.name, sizeof(input_attr.name)))
              << " input_type=" << get_type_string(input_attr.type)
              << " input_fmt=" << get_format_string(input_attr.fmt)
              << " input_elems=" << input_attr.n_elems
              << " submitted_input_type=FP32 output_name_hex="
              << hex_encode(bounded_string(output_attr.name, sizeof(output_attr.name)))
              << " output_type=" << get_type_string(output_attr.type)
              << " output_fmt=" << get_format_string(output_attr.fmt)
              << " output_elems=" << output_attr.n_elems
              << " requested_output_type=FP32\n";
    validate_tensor_contract(input_attr, output_attr);

    for (std::size_t index = 0; index < options.warmup; ++index) {
        static_cast<void>(infer_one(context.get(), input_attr, corpus[index % corpus.size()]));
    }

    std::ofstream output(options.output_path, std::ios::binary | std::ios::trunc);
    if (!output) {
        fail("cannot create output CSV: " + options.output_path);
    }
    output << kOutputHeader << '\n';
    double maximum_error = 0.0;
    int maximum_command_delta = 0;
    std::size_t exact_commands = 0;
    for (const auto &record : corpus) {
        const InferenceResult result = infer_one(context.get(), input_attr, record);
        const std::uint16_t command = actuator_command(result.output);
        const float expected_output = float_from_bits(record.expected_output_bits);
        maximum_error = std::max(maximum_error,
                                 std::abs(static_cast<double>(result.output) - expected_output));
        const int command_delta =
            static_cast<int>(command) - static_cast<int>(record.expected_actuator_permille);
        maximum_command_delta = std::max(maximum_command_delta, std::abs(command_delta));
        exact_commands += command_delta == 0 ? 1U : 0U;

        output << record.index;
        for (const std::uint32_t bits : record.input_bits) {
            output << ',';
            write_hex32(output, bits);
        }
        output << ',';
        write_hex32(output, record.expected_output_bits);
        output << ',' << record.expected_actuator_permille << ',';
        write_hex32(output, float_bits(result.output));
        output << ',' << command << ',' << result.wall_ns << ',' << result.device_us << '\n';
        if ((record.index + 1) % 1000 == 0) {
            std::cout << "IVC_RKNN_PROGRESS completed=" << record.index + 1 << '\n';
        }
    }
    output.flush();
    if (!output) {
        fail("failed while writing output CSV");
    }
    output.close();
    if (!output) {
        fail("failed while closing output CSV");
    }
    if (maximum_error > kMaximumF32Error || maximum_command_delta > kMaximumActuatorDelta) {
        fail("physical output violates the pre-registered FP16 gate");
    }
    require_status(context.destroy(), "destroy final RKNN context");
    finalize_lifecycle_evidence(lifecycle);
    write_resource_evidence(options, lifecycle);
    write_resource_markers(options, lifecycle);

    const auto final_init_us =
        std::chrono::duration_cast<std::chrono::microseconds>(init_finished - init_started).count();
    const auto init_us =
        lifecycle.cold_init_us >= 0 ? lifecycle.cold_init_us : final_init_us;
    std::cout << std::setprecision(17);
    if (options.evidence_marker_copies == 1) {
        write_redundant_marker(options, [&] {
            std::cout << "IVC_RKNN_LINUX_RESULT status=pass vectors=" << corpus.size()
                      << " warmup=" << options.warmup
                      << " core_mask=" << options.core_mask_name << " init_us=" << init_us
                      << " exact_actuator_matches=" << exact_commands
                      << " maximum_absolute_error=" << maximum_error
                      << " maximum_absolute_actuator_delta=" << maximum_command_delta
                      << " perf_query_errors=0 run_errors=0";
        });
    } else {
        write_redundant_marker(options, [&] {
            std::cout << "IVC_RKNN_RESULT_META status=pass vectors=" << corpus.size()
                      << " warmup=" << options.warmup
                      << " core_mask=" << options.core_mask_name << " init_us=" << init_us;
        });
        write_redundant_marker(options, [&] {
            std::cout << "IVC_RKNN_RESULT_ACCURACY exact_actuator_matches=" << exact_commands
                      << " maximum_absolute_actuator_delta=" << maximum_command_delta;
        });
        write_redundant_marker(options, [&] {
            std::cout << "IVC_RKNN_RESULT_ERROR maximum_absolute_error=" << maximum_error;
        });
        write_redundant_marker(options, [&] {
            std::cout << "IVC_RKNN_RESULT_HEALTH perf_query_errors=0 run_errors=0";
        });
    }
    std::cout << "IVC_RKNN_LINUX_DONE\n";
    return 0;
}

} // namespace

int main(int argc, char **argv) {
    try {
        return run(parse_options(argc, argv));
    } catch (const std::exception &error) {
        std::cerr << "IVC_RKNN_LINUX_ERROR message_hex=" << hex_encode(error.what()) << '\n';
        return 1;
    }
}
