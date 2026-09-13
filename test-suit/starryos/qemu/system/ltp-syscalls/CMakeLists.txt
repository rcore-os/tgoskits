cmake_minimum_required(VERSION 3.20)
project(ltp-syscalls C)

# Deterministic coverage for worker teardown required by LTP mount cleanup.
# This helper runs in the native phase and is not an upstream LTP testcase.
add_executable(ltp-isolation-exit-fs src/exit_fs_context.c)
target_compile_options(ltp-isolation-exit-fs PRIVATE -Wall -Wextra -Werror)
install(TARGETS ltp-isolation-exit-fs RUNTIME DESTINATION usr/bin/starry-test-suit)

# Access must distinguish real credentials, mount flags and shared filesystem state.
add_executable(ltp-isolation-access-context src/access_context.c)
target_compile_options(ltp-isolation-access-context PRIVATE -Wall -Wextra -Werror)
install(TARGETS ltp-isolation-access-context RUNTIME DESTINATION usr/bin/starry-test-suit)

if(NOT DEFINED STARRY_STAGING_ROOT)
    message(FATAL_ERROR "STARRY_STAGING_ROOT must be provided by axbuild")
endif()
file(REAL_PATH "${STARRY_STAGING_ROOT}/sbin/mkfs.ext4" LTP_MKFS_EXT4)
if(NOT EXISTS "${LTP_MKFS_EXT4}")
    message(FATAL_ERROR "mkfs.ext4 is missing from the staging root")
endif()
# Install the executable itself so axbuild also discovers its shared libraries.
install(PROGRAMS "${LTP_MKFS_EXT4}" DESTINATION sbin RENAME mkfs.ext4)
install(FILES "${STARRY_STAGING_ROOT}/etc/mke2fs.conf" DESTINATION etc)

set(LTP_SYSCALLS_CASES_FILE "${CMAKE_CURRENT_LIST_DIR}/cases.txt")
file(STRINGS "${LTP_SYSCALLS_CASES_FILE}" LTP_SYSCALLS_CASES
    REGEX "^[A-Za-z0-9_]+$")

# Legacy syscall numbers are only meaningful on architectures exposing them.
string(REGEX MATCH "^[^-]+" LTP_TARGET_ARCH "${CMAKE_C_COMPILER_TARGET}")
set(LTP_ARCH_CASES_FILE "${CMAKE_CURRENT_LIST_DIR}/cases-${LTP_TARGET_ARCH}.txt")
if(EXISTS "${LTP_ARCH_CASES_FILE}")
    file(STRINGS "${LTP_ARCH_CASES_FILE}" LTP_ARCH_CASES
        REGEX "^[A-Za-z0-9_]+$")
    list(APPEND LTP_SYSCALLS_CASES ${LTP_ARCH_CASES})
    list(REMOVE_DUPLICATES LTP_SYSCALLS_CASES)
    list(SORT LTP_SYSCALLS_CASES)
endif()

if(NOT LTP_SYSCALLS_CASES)
    message(FATAL_ERROR "the frozen PR #1775 LTP manifest is empty")
endif()

# Keep source-derived completion contracts separate from the generated case set.
file(STRINGS "${CMAKE_CURRENT_LIST_DIR}/minimum-passes.txt" LTP_PASS_CONTRACTS)
foreach(LTP_PASS_CONTRACT IN LISTS LTP_PASS_CONTRACTS)
    string(STRIP "${LTP_PASS_CONTRACT}" LTP_PASS_CONTRACT)
    if(LTP_PASS_CONTRACT STREQUAL "" OR LTP_PASS_CONTRACT MATCHES "^#")
        continue()
    endif()
    if(NOT LTP_PASS_CONTRACT MATCHES "^([A-Za-z0-9_]+) +([1-9][0-9]*)$")
        message(FATAL_ERROR "invalid LTP completion contract: ${LTP_PASS_CONTRACT}")
    endif()
    set(LTP_MIN_PASSES_${CMAKE_MATCH_1} "${CMAKE_MATCH_2}")
endforeach()

# A total pass count must not hide a silently omitted filesystem.
file(STRINGS "${CMAKE_CURRENT_LIST_DIR}/filesystem-passes.txt" LTP_FS_CONTRACTS)
foreach(LTP_FS_CONTRACT IN LISTS LTP_FS_CONTRACTS)
    string(STRIP "${LTP_FS_CONTRACT}" LTP_FS_CONTRACT)
    if(LTP_FS_CONTRACT STREQUAL "" OR LTP_FS_CONTRACT MATCHES "^#")
        continue()
    endif()
    if(NOT LTP_FS_CONTRACT MATCHES "^([A-Za-z0-9_]+) +([A-Za-z0-9_]+) +([1-9][0-9]*)$")
        message(FATAL_ERROR "invalid LTP filesystem contract: ${LTP_FS_CONTRACT}")
    endif()
    string(APPEND LTP_FILESYSTEM_PASSES_${CMAKE_MATCH_1} " ${CMAKE_MATCH_2}:${CMAKE_MATCH_3}")
endforeach()

set(LTP_SYSCALLS_GENERATED_DIR "${CMAKE_CURRENT_BINARY_DIR}/generated")
file(MAKE_DIRECTORY "${LTP_SYSCALLS_GENERATED_DIR}")

foreach(LTP_CASE_ID IN LISTS LTP_SYSCALLS_CASES)
    if(NOT LTP_CASE_ID MATCHES "^[A-Za-z0-9_]+$")
        message(FATAL_ERROR "invalid LTP testcase id: ${LTP_CASE_ID}")
    endif()
    set(LTP_FILESYSTEM_PASSES "${LTP_FILESYSTEM_PASSES_${LTP_CASE_ID}}")
    set(LTP_MIN_PASSES 1)
    if(DEFINED LTP_MIN_PASSES_${LTP_CASE_ID})
        set(LTP_MIN_PASSES "${LTP_MIN_PASSES_${LTP_CASE_ID}}")
    endif()
    set(LTP_CASE_WRAPPER
        "${LTP_SYSCALLS_GENERATED_DIR}/ltp-syscalls-${LTP_CASE_ID}")
    configure_file(wrapper.sh.in "${LTP_CASE_WRAPPER}" @ONLY)
    install(PROGRAMS "${LTP_CASE_WRAPPER}"
        DESTINATION usr/bin/starry-test-suit)
endforeach()
