cmake_minimum_required(VERSION 3.20)

project(bug_robust_mutex_owner_death C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug_robust_mutex_owner_death src/main.c)
target_compile_options(bug_robust_mutex_owner_death PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug_robust_mutex_owner_death RUNTIME DESTINATION usr/bin)
