cmake_minimum_required(VERSION 3.20)
project(block-io-smoke C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

set(BLOCK_IO_SMOKE_DATA "${CMAKE_CURRENT_BINARY_DIR}/block-io-smoke.dat")
add_custom_command(
    OUTPUT "${BLOCK_IO_SMOKE_DATA}"
    COMMAND dd if=/dev/zero of=${BLOCK_IO_SMOKE_DATA} bs=4096 count=1
    VERBATIM
)
add_custom_target(block-io-smoke-data ALL DEPENDS "${BLOCK_IO_SMOKE_DATA}")

add_executable(block-io-smoke src/main.c)
target_compile_options(block-io-smoke PRIVATE -Wall -Wextra -Werror)
add_dependencies(block-io-smoke block-io-smoke-data)

install(TARGETS block-io-smoke RUNTIME DESTINATION usr/bin/starry-test-suit)
install(FILES "${BLOCK_IO_SMOKE_DATA}" DESTINATION root)
