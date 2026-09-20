cmake_minimum_required(VERSION 3.20)
project(test-case-task-isolation C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(test-case-task-isolation-1-leak src/leak.c)
add_executable(test-case-task-isolation-2-verify src/verify.c)
target_compile_options(test-case-task-isolation-1-leak PRIVATE -Wall -Wextra -Werror)
target_compile_options(test-case-task-isolation-2-verify PRIVATE -Wall -Wextra -Werror)

install(
    TARGETS
        test-case-task-isolation-1-leak
        test-case-task-isolation-2-verify
    RUNTIME DESTINATION usr/bin/starry-test-suit)
