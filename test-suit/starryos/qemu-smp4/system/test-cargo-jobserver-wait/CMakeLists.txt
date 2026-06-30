cmake_minimum_required(VERSION 3.20)
project(test-cargo-jobserver-wait C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(test-cargo-jobserver-wait src/main.c)
target_compile_options(test-cargo-jobserver-wait PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-cargo-jobserver-wait PRIVATE pthread)

install(TARGETS test-cargo-jobserver-wait RUNTIME DESTINATION usr/bin/starry-test-suit)
