#include "k230_sdk_compat.h"

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <exception>

#include <nncase/runtime/interpreter.h>
#include <nncase/runtime/runtime_op_utility.h>

using namespace nncase;
using namespace nncase::runtime;
using namespace nncase::runtime::detail;

static void print_shape(const dims_t &shape) {
    std::printf("[");
    for (size_t i = 0; i < shape.size(); ++i) {
        if (i) {
            std::printf(",");
        }
        std::printf("%zu", shape[i]);
    }
    std::printf("]");
}

static uint8_t *read_file(const char *path, size_t *size) {
    FILE *fp = std::fopen(path, "rb");
    if (!fp) {
        return nullptr;
    }
    if (std::fseek(fp, 0, SEEK_END) != 0) {
        std::fclose(fp);
        return nullptr;
    }
    long len = std::ftell(fp);
    if (len <= 0) {
        std::fclose(fp);
        return nullptr;
    }
    std::rewind(fp);
    auto *data = static_cast<uint8_t *>(std::malloc(static_cast<size_t>(len)));
    if (!data) {
        std::fclose(fp);
        return nullptr;
    }
    size_t got = std::fread(data, 1, static_cast<size_t>(len), fp);
    std::fclose(fp);
    if (got != static_cast<size_t>(len)) {
        std::free(data);
        return nullptr;
    }
    *size = static_cast<size_t>(len);
    return data;
}

static void fill_input(runtime_tensor &tensor, size_t input_index) {
    auto mapped = tensor.impl()
                      ->to_host()
                      .unwrap()
                      ->buffer()
                      .as_host()
                      .unwrap()
                      .map(map_access_::map_write)
                      .unwrap()
                      .buffer();
    auto *data = reinterpret_cast<uint8_t *>(mapped.data());
    for (size_t i = 0; i < mapped.size_bytes(); ++i) {
        data[i] = static_cast<uint8_t>((i + input_index * 17) & 0xff);
    }
    hrt::sync(tensor, sync_op_t::sync_write_back, true)
        .expect("sync input failed");
}

static uint64_t digest_output(runtime_tensor &tensor, size_t &bytes) {
    auto mapped = tensor.impl()
                      ->to_host()
                      .unwrap()
                      ->buffer()
                      .as_host()
                      .unwrap()
                      .map(map_access_::map_read)
                      .unwrap()
                      .buffer();
    bytes = mapped.size_bytes();
    const auto *data = reinterpret_cast<const uint8_t *>(mapped.data());
    uint64_t hash = 1469598103934665603ULL;
    for (size_t i = 0; i < bytes; ++i) {
        hash ^= data[i];
        hash *= 1099511628211ULL;
    }
    return hash;
}

static int run_minimal(const char *kmodel_path) {
    if (k230_compat_init() != 0) {
        std::printf("NNCASE_MINIMAL_FAIL: cannot initialize /dev/kpu compat\n");
        return 1;
    }

    size_t model_size = 0;
    uint8_t *model_data = read_file(kmodel_path, &model_size);
    if (!model_data) {
        std::printf("NNCASE_MINIMAL_FAIL: cannot read kmodel: %s\n", kmodel_path);
        return 1;
    }

    interpreter interp;
    std::printf("NNCASE_MINIMAL: loading kmodel path=%s bytes=%zu\n", kmodel_path,
                model_size);
    gsl::span<const gsl::byte> model_span(
        reinterpret_cast<const gsl::byte *>(model_data), model_size);
    interp.load_model(model_span, true).expect("load_model failed");
    std::printf("NNCASE_MINIMAL: load_model ok\n");
    std::printf("NNCASE_MINIMAL: model io inputs=%zu outputs=%zu\n",
                interp.inputs_size(), interp.outputs_size());

    for (size_t i = 0; i < interp.inputs_size(); ++i) {
        auto desc = interp.input_desc(i);
        auto shape = interp.input_shape(i);
        auto tensor =
            host_runtime_tensor::create(desc.datatype, shape, hrt::pool_shared)
                .expect("cannot create input tensor");
        std::printf("NNCASE_MINIMAL: input[%zu] datatype=%u shape=", i,
                    static_cast<unsigned>(desc.datatype));
        print_shape(shape);
        std::printf(" bytes=%zu\n",
                    tensor.impl()->to_host().unwrap()->buffer().size_bytes());
        fill_input(tensor, i);
        interp.input_tensor(i, tensor).expect("cannot set input tensor");
    }

    for (size_t i = 0; i < interp.outputs_size(); ++i) {
        auto desc = interp.output_desc(i);
        auto shape = interp.output_shape(i);
        auto tensor =
            host_runtime_tensor::create(desc.datatype, shape, hrt::pool_shared)
                .expect("cannot create output tensor");
        std::printf("NNCASE_MINIMAL: output[%zu] datatype=%u shape=", i,
                    static_cast<unsigned>(desc.datatype));
        print_shape(shape);
        std::printf(" elements=%zu\n", compute_size(shape));
        interp.output_tensor(i, tensor).expect("cannot set output tensor");
    }

    std::printf("NNCASE_MINIMAL: running nncase interpreter\n");
    interp.run().expect("interp.run failed");
    std::printf("NNCASE_MINIMAL: interp.run done\n");

    for (size_t i = 0; i < interp.outputs_size(); ++i) {
        auto tensor = interp.output_tensor(i).expect("cannot get output tensor");
        size_t bytes = 0;
        auto hash = digest_output(tensor, bytes);
        std::printf("NNCASE_MINIMAL: output[%zu] bytes=%zu fnv1a64=0x%016llx\n",
                    i, bytes, static_cast<unsigned long long>(hash));
    }

    std::free(model_data);
    k230_compat_dump_stats();
    std::printf("NNCASE_MINIMAL_PASS\n");
    std::fflush(nullptr);
    // The official K230 SDK MMZ allocator can assert while tearing down process
    // globals under Starry/Linux ABI. The run has completed once PASS is printed.
    std::_Exit(0);
    return 0;
}

int main(int argc, char *argv[]) {
    if (argc != 2) {
        std::printf("NNCASE_MINIMAL_FAIL: usage: %s <kmodel>\n", argv[0]);
        return 2;
    }
    try {
        return run_minimal(argv[1]);
    } catch (const std::exception &ex) {
        std::printf("NNCASE_MINIMAL_FAIL: exception: %s\n", ex.what());
        return 1;
    } catch (...) {
        std::printf("NNCASE_MINIMAL_FAIL: unknown exception\n");
        return 1;
    }
}
