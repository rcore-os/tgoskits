cmake_minimum_required(VERSION 3.20)

project(bug-tgkill-sigwait-ignored C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bug-tgkill-sigwait-ignored src/main.c)
target_compile_options(bug-tgkill-sigwait-ignored PRIVATE -Wall -Wextra -Werror)

install(TARGETS bug-tgkill-sigwait-ignored RUNTIME DESTINATION usr/bin)
