cmake_minimum_required(VERSION 3.20)
project(kmod-modules C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(kmod-load src/main.c)
target_compile_options(kmod-load PRIVATE -Wall -Wextra -Werror)
install(TARGETS kmod-load RUNTIME DESTINATION usr/bin)

# The `.ko` images are loadable kernel modules — they are *generated* from the
# crates under `os/StarryOS/modules/` by `cargo xtask starry kmod build`, not
# checked into the tree. Build them here (configure time) and install them into
# the guest rootfs so the loader can finit_module(2) them.
get_filename_component(WORKSPACE_ROOT
    "${CMAKE_CURRENT_SOURCE_DIR}/../../../../../.." ABSOLUTE)

# Map the cross toolchain's processor to the `cargo xtask` arch name.
if(CMAKE_SYSTEM_PROCESSOR MATCHES "x86_64|AMD64|amd64")
    set(KMOD_ARCH x86_64)
elseif(CMAKE_SYSTEM_PROCESSOR MATCHES "aarch64|arm64")
    set(KMOD_ARCH aarch64)
elseif(CMAKE_SYSTEM_PROCESSOR MATCHES "riscv64")
    set(KMOD_ARCH riscv64)
elseif(CMAKE_SYSTEM_PROCESSOR MATCHES "loongarch64")
    set(KMOD_ARCH loongarch64)
else()
    set(KMOD_ARCH x86_64)
endif()

message(STATUS "kmod-modules: building .ko via cargo xtask (arch=${KMOD_ARCH})")
# Clear the guest cross-build pkg-config redirection before shelling out to
# cargo: building the host `xtask` binary pulls in host crates (e.g.
# `libudev-sys`) whose build scripts run pkg-config, and the guest sysroot has
# no host libraries. Unsetting these restores host pkg-config behaviour.
execute_process(
    COMMAND "${CMAKE_COMMAND}" -E env
            --unset=PKG_CONFIG_LIBDIR
            --unset=PKG_CONFIG_SYSROOT_DIR
            --unset=PKG_CONFIG_PATH
            cargo xtask starry kmod build --all --arch ${KMOD_ARCH}
    WORKING_DIRECTORY "${WORKSPACE_ROOT}"
    RESULT_VARIABLE KMOD_BUILD_RESULT)
if(NOT KMOD_BUILD_RESULT EQUAL 0)
    message(FATAL_ERROR
        "cargo xtask starry kmod build failed (exit ${KMOD_BUILD_RESULT})")
endif()

install(FILES "${WORKSPACE_ROOT}/target/${KMOD_ARCH}/kmod/hello.ko"
        DESTINATION lib/modules)
