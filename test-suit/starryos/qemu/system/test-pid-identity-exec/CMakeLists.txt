cmake_minimum_required(VERSION 3.20)
project(test-pid-identity-exec C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-pid-identity-exec src/main.c)
target_include_directories(test-pid-identity-exec PRIVATE src ${CMAKE_CURRENT_SOURCE_DIR}/../common)
target_compile_options(test-pid-identity-exec PRIVATE -Wall -Wextra -Werror -pthread)
target_link_options(test-pid-identity-exec PRIVATE -static -pthread)

install(TARGETS test-pid-identity-exec RUNTIME DESTINATION usr/bin/starry-test-suit)
