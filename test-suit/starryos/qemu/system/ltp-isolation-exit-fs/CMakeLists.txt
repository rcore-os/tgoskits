cmake_minimum_required(VERSION 3.20)
project(ltp-isolation-exit-fs C)

add_executable(
    ltp-isolation-exit-fs
    "${CMAKE_CURRENT_LIST_DIR}/../ltp-syscalls/src/exit_fs_context.c"
)
target_compile_options(ltp-isolation-exit-fs PRIVATE -Wall -Wextra -Werror)
install(TARGETS ltp-isolation-exit-fs RUNTIME DESTINATION usr/bin/starry-test-suit)

if(NOT DEFINED STARRY_STAGING_ROOT)
    message(FATAL_ERROR "STARRY_STAGING_ROOT must be provided by axbuild")
endif()
file(REAL_PATH "${STARRY_STAGING_ROOT}/sbin/mkfs.ext4" LTP_MKFS_EXT4)
if(NOT EXISTS "${LTP_MKFS_EXT4}")
    message(FATAL_ERROR "mkfs.ext4 is missing from the staging root")
endif()
if(NOT DEFINED STARRY_SYSTEM_SELECTED_SUBCASES
    OR NOT "ltp-syscalls" IN_LIST STARRY_SYSTEM_SELECTED_SUBCASES)
    install(PROGRAMS "${LTP_MKFS_EXT4}" DESTINATION sbin RENAME mkfs.ext4)
    install(FILES "${STARRY_STAGING_ROOT}/etc/mke2fs.conf" DESTINATION etc)
endif()
