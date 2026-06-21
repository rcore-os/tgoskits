cmake_minimum_required(VERSION 3.20)
project(bug-dir-cookie-unlink-rmdir C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bug-dir-cookie-unlink-rmdir src/main.c)
target_compile_options(bug-dir-cookie-unlink-rmdir PRIVATE -Wall -Wextra -Werror)
install(TARGETS bug-dir-cookie-unlink-rmdir RUNTIME DESTINATION usr/bin/starry-test-suit)
