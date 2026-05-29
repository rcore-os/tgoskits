cmake_minimum_required(VERSION 3.20)
project(k230-nncase-runtime C)

set(K230_SDK_ROOT "" CACHE PATH "Path to the checked out K230 SDK")
if(NOT K230_SDK_ROOT)
    set(_probe "${CMAKE_CURRENT_SOURCE_DIR}")
    while(NOT _probe STREQUAL "/")
        if(EXISTS "${_probe}/target/official-k230/k230-sdk-src")
            set(K230_SDK_ROOT "${_probe}/target/official-k230/k230-sdk-src")
            break()
        endif()
        get_filename_component(_probe "${_probe}" DIRECTORY)
    endwhile()
endif()

if(NOT K230_SDK_ROOT OR NOT EXISTS "${K230_SDK_ROOT}/src/big/nncase/riscv64/nncase/lib/libNncase.Runtime.Native.a")
    message(FATAL_ERROR "K230 SDK NNCase runtime was not found; set -DK230_SDK_ROOT=/path/to/k230-sdk-src")
endif()

set(K230_NNCASE_ROOT "${K230_SDK_ROOT}/src/big/nncase/riscv64")
set(K230_NNCASE_LIB "${K230_NNCASE_ROOT}/nncase/lib")
set(K230_RVV_LIB "${K230_NNCASE_ROOT}/rvvlib")
set(K230_JPEG_ROOT "${K230_SDK_ROOT}/src/big/utils/lib/hhb-prebuilt-decode")
set(K230_KMODEL "${K230_SDK_ROOT}/src/big/kmodel/ai_poc/kmodel/yolov8n_320.kmodel")
set(K230_BUS_JPG "${K230_SDK_ROOT}/src/big/kmodel/ai_poc/images/bus.jpg")
set(K230_PREBUILT_DIR "${CMAKE_CURRENT_SOURCE_DIR}/assets/bin")
set(K230_LINUX_MUSL_PREFIX "" CACHE PATH "Linux-runnable riscv64-linux-musl cross sysroot prefix")

if(NOT EXISTS "${K230_KMODEL}")
    set(K230_KMODEL "${CMAKE_CURRENT_SOURCE_DIR}/../../kpu-smoke/c/assets/kmodels/yolov8n_320.kmodel")
endif()
if(NOT EXISTS "${K230_KMODEL}")
    message(FATAL_ERROR "yolov8n_320.kmodel was not found in the K230 SDK or kpu-smoke assets")
endif()
if(NOT EXISTS "${K230_BUS_JPG}")
    message(FATAL_ERROR "bus.jpg was not found in the K230 SDK image assets")
endif()

set(K230_BUILD_FROM_SOURCE OFF)
if(K230_CXX)
    set(K230_BUILD_FROM_SOURCE ON)
elseif(CMAKE_HOST_SYSTEM_PROCESSOR MATCHES "^(x86_64|amd64|AMD64)$")
    set(K230_CXX "${K230_SDK_ROOT}/toolchain/riscv64-linux-musleabi_for_x86_64-pc-linux-gnu/bin/riscv64-unknown-linux-musl-g++")
    if(EXISTS "${K230_CXX}")
        set(K230_BUILD_FROM_SOURCE ON)
    endif()
endif()

if(K230_BUILD_FROM_SOURCE)
    message(STATUS "Building K230 NNCase runtime demo binaries from source")
    message(STATUS "Using K230 NNCase C++ compiler: ${K230_CXX}")

    if(NOT K230_LINUX_MUSL_PREFIX)
        if(EXISTS "/linux-musl-cross/riscv64-linux-musl/lib/crt1.o")
            set(K230_LINUX_MUSL_PREFIX "/linux-musl-cross")
        elseif(EXISTS "/opt/riscv64-linux-musl-cross/riscv64-linux-musl/lib/crt1.o")
            set(K230_LINUX_MUSL_PREFIX "/opt/riscv64-linux-musl-cross")
        endif()
    endif()

    set(K230_LINUX_MUSL_SYSROOT "${K230_LINUX_MUSL_PREFIX}/riscv64-linux-musl")
    if(NOT EXISTS "${K230_LINUX_MUSL_SYSROOT}/lib/crt1.o"
            OR NOT EXISTS "${K230_LINUX_MUSL_SYSROOT}/lib/libc.a")
        message(FATAL_ERROR "Linux riscv64-musl sysroot was not found; set -DK230_LINUX_MUSL_PREFIX=/path/to/riscv64-linux-musl-cross")
    endif()

    file(GLOB K230_LINUX_MUSL_GCC_LIB_CANDIDATES LIST_DIRECTORIES true
        "${K230_LINUX_MUSL_PREFIX}/lib/gcc/riscv64-linux-musl/*")
    list(SORT K230_LINUX_MUSL_GCC_LIB_CANDIDATES)
    list(REVERSE K230_LINUX_MUSL_GCC_LIB_CANDIDATES)
    list(GET K230_LINUX_MUSL_GCC_LIB_CANDIDATES 0 K230_LINUX_MUSL_GCC_LIB)
    if(NOT EXISTS "${K230_LINUX_MUSL_GCC_LIB}/libgcc.a")
        message(FATAL_ERROR "Linux riscv64-musl libgcc was not found under ${K230_LINUX_MUSL_PREFIX}")
    endif()

    get_filename_component(K230_SDK_TOOLCHAIN_ROOT "${K230_CXX}" DIRECTORY)
    get_filename_component(K230_SDK_TOOLCHAIN_ROOT "${K230_SDK_TOOLCHAIN_ROOT}" DIRECTORY)
    set(K230_SDK_GCC_LIB "${K230_SDK_TOOLCHAIN_ROOT}/lib/gcc/riscv64-unknown-linux-musl/12.0.1")
    set(K230_SDK_TARGET_LIB "${K230_SDK_TOOLCHAIN_ROOT}/riscv64-unknown-linux-musl/lib")
    if(NOT EXISTS "${K230_SDK_GCC_LIB}/crtbeginT.o"
            OR NOT EXISTS "${K230_SDK_TARGET_LIB}/libstdc++.a")
        message(FATAL_ERROR "K230 SDK target C++ runtime libraries were not found under ${K230_SDK_TOOLCHAIN_ROOT}")
    endif()

    set(K230_COMMON_SOURCES
        "${CMAKE_CURRENT_SOURCE_DIR}/src/k230_sdk_compat.cc"
    )
    set(K230_COMMON_INCLUDES
        "-I${CMAKE_CURRENT_SOURCE_DIR}/src"
        "-I${CMAKE_CURRENT_SOURCE_DIR}/../../../../../../drivers/npu/k230-kpu/include"
        "-I${K230_NNCASE_ROOT}"
        "-I${K230_NNCASE_ROOT}/nncase/include"
        "-I${K230_NNCASE_ROOT}/rvvlib/include"
        "-I${K230_SDK_ROOT}/src/big/mpp/userapps/api"
        "-I${K230_SDK_ROOT}/src/big/mpp/include"
        "-I${K230_SDK_ROOT}/src/big/mpp/include/comm"
        "-I${K230_JPEG_ROOT}/include"
    )
    set(K230_COMMON_FLAGS
        "-std=gnu++17"
        "--sysroot=${K230_LINUX_MUSL_SYSROOT}"
        "-O2"
        "-g0"
        "-Wall"
        "-Wextra"
        "-Wno-unused-parameter"
        "-Wno-missing-field-initializers"
        "-fdiagnostics-color=never"
    )
    set(K230_LINK_START_FILES
        "-nostdlib"
        "-static"
        "-Wl,-e,_start"
        "${K230_LINUX_MUSL_SYSROOT}/lib/crt1.o"
        "${K230_LINUX_MUSL_SYSROOT}/lib/crti.o"
        "${K230_SDK_GCC_LIB}/crtbeginT.o"
    )
    set(K230_LINK_ARGS
        "-Wl,--wrap=gnne_enable"
        "-Wl,--wrap=open"
        "-Wl,--wrap=openat"
        "-Wl,--wrap=close"
        "-Wl,--wrap=mmap"
        "-Wl,--wrap=munmap"
        "-Wl,--wrap=ioctl"
        "-L${K230_NNCASE_LIB}"
        "-L${K230_RVV_LIB}"
        "-L${K230_JPEG_ROOT}/lib"
        "-L${K230_LINUX_MUSL_SYSROOT}/lib"
        "-L${K230_LINUX_MUSL_GCC_LIB}"
        "-L${K230_SDK_GCC_LIB}"
        "-L${K230_SDK_TARGET_LIB}"
        "-Wl,--start-group"
        "-lrvv"
        "-lNncase.Runtime.Native"
        "-lnncase.rt_modules.k230"
        "-lfunctional_k230"
        "-ljpeg"
        "-lz"
        "-lstdc++"
        "-lsupc++"
        "-lm"
        "-lc"
        "-lgcc"
        "-lgcc_eh"
        "-latomic"
        "-Wl,--end-group"
        "${K230_SDK_GCC_LIB}/crtend.o"
        "${K230_LINUX_MUSL_SYSROOT}/lib/crtn.o"
    )

    function(add_k230_sdk_binary name)
        set(output "${CMAKE_CURRENT_BINARY_DIR}/${name}")
        add_custom_command(
            OUTPUT "${output}"
            COMMAND "${K230_CXX}"
                    ${K230_COMMON_FLAGS}
                    ${K230_COMMON_INCLUDES}
                    ${K230_LINK_START_FILES}
                    ${ARGN}
                    ${K230_COMMON_SOURCES}
                    "-o" "${output}"
                    ${K230_LINK_ARGS}
            DEPENDS ${ARGN} ${K230_COMMON_SOURCES}
            VERBATIM
            COMMAND_EXPAND_LISTS
        )
        add_custom_target("${name}-target" ALL DEPENDS "${output}")
        install(PROGRAMS "${output}" DESTINATION usr/bin)
    endfunction()

    add_k230_sdk_binary(kpu-nncase-minimal
        "${CMAKE_CURRENT_SOURCE_DIR}/src/kpu_nncase_minimal.cc"
    )
    add_k230_sdk_binary(k230-yolov8n-demo
        "${CMAKE_CURRENT_SOURCE_DIR}/src/k230_yolov8n_demo.cc"
    )
else()
    if(EXISTS "${K230_PREBUILT_DIR}/kpu-nncase-minimal" AND EXISTS "${K230_PREBUILT_DIR}/k230-yolov8n-demo")
        message(STATUS "Using prebuilt K230 NNCase runtime demo binaries from ${K230_PREBUILT_DIR}")
        install(PROGRAMS
            "${K230_PREBUILT_DIR}/kpu-nncase-minimal"
            "${K230_PREBUILT_DIR}/k230-yolov8n-demo"
            DESTINATION usr/bin
        )
    else()
        message(FATAL_ERROR "K230 NNCase runtime prebuilt binaries are missing. Run c/tools/build-nncase-runtime-binaries.sh from this case directory, or pass -DK230_CXX=/path/to/riscv64-unknown-linux-musl-g++ on an amd64 K230 SDK environment.")
    endif()
endif()

install(PROGRAMS src/run-nncase-runtime-demo.sh DESTINATION usr/bin RENAME k230-nncase-runtime-demo)
install(FILES "${K230_KMODEL}" DESTINATION usr/share/k230-nncase-runtime/models)
install(FILES "${K230_BUS_JPG}" DESTINATION usr/share/k230-nncase-runtime/images)
