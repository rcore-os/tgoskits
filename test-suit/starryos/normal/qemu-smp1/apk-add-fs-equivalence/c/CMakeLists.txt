cmake_minimum_required(VERSION 3.20)

project(apk-add-fs-equivalence C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(apk-add-fs-equivalence src/main.c)
target_compile_options(apk-add-fs-equivalence PRIVATE -Wall -Wextra -Werror)

install(TARGETS apk-add-fs-equivalence RUNTIME DESTINATION usr/bin)
