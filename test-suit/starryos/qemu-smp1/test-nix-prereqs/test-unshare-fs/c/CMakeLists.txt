cmake_minimum_required(VERSION 3.16)
project(test-unshare-fs C)
set(APP_NAME test-unshare-fs)

add_executable(${APP_NAME} src/main.c)
target_compile_options(${APP_NAME} PRIVATE -Wall -Wextra -Werror)
install(TARGETS ${APP_NAME} RUNTIME DESTINATION usr/bin/starry-test-suit)
