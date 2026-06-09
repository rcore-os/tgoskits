cmake_minimum_required(VERSION 3.20)
project(test-dup-fcntl C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
add_executable(test-dup-fcntl
    src/main.c
    src/test_helpers.c
    src/parts_dup_dup3_fcntl.c
    src/parts_flock_basic.c
    src/parts_fcntl_lock.c
    src/parts_fcntl_rdlck.c
    src/parts_flock_extra.c
)
target_include_directories(test-dup-fcntl PRIVATE src)
target_compile_options(test-dup-fcntl PRIVATE -Wall -Wextra -Werror)
install(TARGETS test-dup-fcntl RUNTIME DESTINATION usr/bin/starry-test-suit)
