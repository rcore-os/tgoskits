cmake_minimum_required(VERSION 3.16)
project(test-open-unlink-write C)
set(APP_NAME test-open-unlink-write)

add_executable(${APP_NAME} src/main.c)
target_compile_options(${APP_NAME} PRIVATE -Wall -Wextra -Werror)
install(TARGETS ${APP_NAME} RUNTIME DESTINATION usr/bin/starry-test-suit)
