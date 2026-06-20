#!/bin/sh

set -u

unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY all_proxy

echo "APK_CMAKE_STABLE_TEST_BEGIN"

cleanup() {
    echo "APK_CMAKE_CLEANUP_BEGIN"

    rm -rf \
        /tmp/cmake-src \
        /tmp/cmake-build \
        /root/.cache

    sync || true

    echo "APK_CMAKE_CLEANUP_DONE"
}

trap cleanup EXIT INT TERM

check_required_tools() {
    missing=0

    for tool in cmake ctest cc c++ make; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            echo "APK_CMAKE_MISSING_TOOL_${tool}"
            missing=1
        fi
    done

    if [ "$missing" -ne 0 ]; then
        echo "APK_CMAKE_STABLE_TEST_FAILED"
        return 1
    fi

    return 0
}

write_cmake_project() {
    rm -rf /tmp/cmake-src /tmp/cmake-build
    mkdir -p /tmp/cmake-src

    cat > /tmp/cmake-src/CMakeLists.txt <<'CMEOF'
cmake_minimum_required(VERSION 3.16)

project(apk_cmake_stable_test C CXX)

add_library(mylib STATIC mylib.c)

target_include_directories(
    mylib
    PUBLIC
    ${CMAKE_CURRENT_SOURCE_DIR}
)

add_executable(hello_c main.c)

target_link_libraries(
    hello_c
    PRIVATE
    mylib
)

add_executable(hello_cpp main.cpp)

enable_testing()

add_test(
    NAME hello_c_runs
    COMMAND hello_c
)

add_test(
    NAME hello_cpp_runs
    COMMAND hello_cpp
)
CMEOF

    cat > /tmp/cmake-src/mylib.h <<'CMEOF'
#pragma once

int add(int a, int b);
CMEOF

    cat > /tmp/cmake-src/mylib.c <<'CMEOF'
#include "mylib.h"

int add(int a, int b) {
    return a + b;
}
CMEOF

    cat > /tmp/cmake-src/main.c <<'CMEOF'
#include <stdio.h>
#include "mylib.h"

int main(void) {
    if (add(2, 3) != 5) {
        return 1;
    }

    puts("hello from CMake C build");
    return 0;
}
CMEOF

    cat > /tmp/cmake-src/main.cpp <<'CMEOF'
#include <iostream>
#include <string>
#include <vector>

int main() {
    std::vector<std::string> words{
        "hello",
        "from",
        "cmake",
        "cpp"
    };

    if (words.size() != 4) {
        return 1;
    }

    std::cout << "hello from CMake C++ build" << std::endl;
    return 0;
}
CMEOF
}

run_cmake_test() {
    label="$1"

    echo "APK_CMAKE_VERSION_BEGIN_${label}"

    cmake --version || {
        echo "APK_CMAKE_VERSION_FAILED_${label}"
        return 1
    }

    ctest --version || {
        echo "APK_CMAKE_CTEST_VERSION_FAILED_${label}"
        return 1
    }

    echo "APK_CMAKE_E_COMMAND_BEGIN_${label}"

    cmake -E echo "hello from cmake" || {
        echo "APK_CMAKE_E_COMMAND_FAILED_${label}"
        return 1
    }

    write_cmake_project

    echo "APK_CMAKE_CONFIGURE_BEGIN_${label}"

    cmake \
        -S /tmp/cmake-src \
        -B /tmp/cmake-build \
        -DCMAKE_BUILD_TYPE=Release || {
            echo "APK_CMAKE_CONFIGURE_FAILED_${label}"
            return 1
        }

    echo "APK_CMAKE_BUILD_BEGIN_${label}"

    cmake \
        --build /tmp/cmake-build \
        --verbose || {
            echo "APK_CMAKE_BUILD_FAILED_${label}"
            return 1
        }

    echo "APK_CMAKE_CTEST_BEGIN_${label}"

    ctest \
        --test-dir /tmp/cmake-build \
        --output-on-failure || {
            echo "APK_CMAKE_CTEST_FAILED_${label}"
            return 1
        }

    echo "APK_CMAKE_RUN_BEGIN_${label}"

    /tmp/cmake-build/hello_c || {
        echo "APK_CMAKE_RUN_C_FAILED_${label}"
        return 1
    }

    /tmp/cmake-build/hello_cpp || {
        echo "APK_CMAKE_RUN_CPP_FAILED_${label}"
        return 1
    }

    echo "APK_CMAKE_REPO_TEST_DONE_${label}"
    return 0
}

if ! check_required_tools; then
    exit 1
fi

if run_cmake_test "prebuilt"; then
    echo "APK_CMAKE_STABLE_TEST_PASSED"
    exit 0
fi

echo "APK_CMAKE_STABLE_TEST_FAILED"
exit 1
