cmake_minimum_required(VERSION 3.20)
project(block-io-bench C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(block-io-bench src/main.c)
target_compile_options(block-io-bench PRIVATE -Wall -Wextra -Werror)

set(BLOCK_IO_BENCH_DATA_FILES)
foreach(idx RANGE 0 2)
    set(data_file "${CMAKE_CURRENT_BINARY_DIR}/block-io-bench-${idx}.dat")
    add_custom_command(
        OUTPUT "${data_file}"
        COMMAND dd if=/dev/zero of=${data_file} bs=1048576 count=1
        VERBATIM
    )
    list(APPEND BLOCK_IO_BENCH_DATA_FILES "${data_file}")
endforeach()

add_custom_target(block-io-bench-data ALL DEPENDS ${BLOCK_IO_BENCH_DATA_FILES})
add_dependencies(block-io-bench block-io-bench-data)

install(TARGETS block-io-bench RUNTIME DESTINATION usr/bin/starry-test-suit)
install(FILES ${BLOCK_IO_BENCH_DATA_FILES} DESTINATION root)
