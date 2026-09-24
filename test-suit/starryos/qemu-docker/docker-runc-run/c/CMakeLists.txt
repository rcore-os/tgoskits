cmake_minimum_required(VERSION 3.20)
project(docker-runc-run C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(docker-runc-run-probe src/main.c)
target_compile_options(docker-runc-run-probe PRIVATE -Wall -Wextra -Werror)
# The probe is cross-built against the Alpine musl toolchain sysroot and runs
# on the Debian glibc rootfs, so it must not carry runtime library deps.
target_link_options(docker-runc-run-probe PRIVATE -static)

if(NOT DEFINED STARRY_STAGING_ROOT)
    message(FATAL_ERROR "STARRY_STAGING_ROOT must be provided by axbuild")
endif()
set(RUNC_BIN "${STARRY_STAGING_ROOT}/usr/bin/runc")
set(BUSYBOX_BIN "${STARRY_STAGING_ROOT}/bin/busybox.static")
foreach(artifact IN ITEMS "${RUNC_BIN}" "${BUSYBOX_BIN}")
    if(NOT EXISTS "${artifact}")
        message(FATAL_ERROR "prebuild artifact missing: ${artifact}")
    endif()
endforeach()

install(TARGETS docker-runc-run-probe RUNTIME DESTINATION usr/bin)
install(PROGRAMS docker-runc-run-smoke.sh DESTINATION usr/bin)
install(PROGRAMS "${RUNC_BIN}" DESTINATION usr/sbin)

# OCI bundles: <dest>/config.json + <dest>/rootfs/busybox. The spec shape
# follows the x-kernel reference bring-up: empty capabilities, no seccomp,
# pid/mount/uts/ipc namespaces, only a /proc mount.
install(FILES config-echo.json DESTINATION opt/hello-bundle RENAME config.json)
install(FILES config-exit.json DESTINATION opt/exit1-bundle RENAME config.json)
install(FILES config-ns.json DESTINATION opt/ns-bundle RENAME config.json)
install(FILES config-pids.json DESTINATION opt/pids-bundle RENAME config.json)
install(PROGRAMS "${BUSYBOX_BIN}" DESTINATION opt/hello-bundle/rootfs RENAME busybox)
install(PROGRAMS "${BUSYBOX_BIN}" DESTINATION opt/exit1-bundle/rootfs RENAME busybox)
install(PROGRAMS "${BUSYBOX_BIN}" DESTINATION opt/ns-bundle/rootfs RENAME busybox)
install(PROGRAMS "${BUSYBOX_BIN}" DESTINATION opt/pids-bundle/rootfs RENAME busybox)
install(PROGRAMS check.sh DESTINATION opt/ns-bundle/rootfs)
install(TARGETS docker-runc-run-probe RUNTIME
        DESTINATION opt/pids-bundle/rootfs RENAME probe)
