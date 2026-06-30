cmake_minimum_required(VERSION 3.20)

project(util-linux-test C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(util-linux-test src/main.c)
target_compile_options(util-linux-test PRIVATE -Wall -Wextra -Werror)

# Create a 4MB ext4 test image at build time using host mkfs.ext4.
# This image is used at runtime for mount -t ext4 testing since
# mkfs.ext4 does not work reliably on the StarryOS guest.
# 4MB is sufficient for ext4 and avoids excessive memory use when the
# kernel buffers the entire image in RAM.
set(EXT4_TEST_IMG "${CMAKE_CURRENT_BINARY_DIR}/test-ext4.img")
add_custom_command(
    OUTPUT "${EXT4_TEST_IMG}"
    COMMAND dd if=/dev/zero of="${EXT4_TEST_IMG}" bs=1M count=4 2>/dev/null
    COMMAND mkfs.ext4 -F -b 4096 -I 256 -O "^metadata_csum,^64bit,^has_journal,^flex_bg" "${EXT4_TEST_IMG}" >/dev/null 2>&1
    COMMENT "Creating ext4 test image"
)
add_custom_target(ext4-img ALL DEPENDS "${EXT4_TEST_IMG}")

install(TARGETS util-linux-test RUNTIME DESTINATION usr/bin)
install(FILES "${EXT4_TEST_IMG}" DESTINATION usr/share/util-linux-test RENAME test-ext4.img)
