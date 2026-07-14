cmake_minimum_required(VERSION 3.20)
project(test-io-getevents C)
set(APP_NAME test-io-getevents)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
add_executable(${APP_NAME} src/main.c)
target_include_directories(${APP_NAME} PRIVATE src ${CMAKE_CURRENT_SOURCE_DIR}/../common)
target_compile_options(${APP_NAME} PRIVATE -Wall -Wextra -Werror)
# Build this stricter probe, but keep it out of the grouped CI pass condition
# until Starry's Linux AIO errno edge cases are complete.
install(TARGETS ${APP_NAME} RUNTIME DESTINATION usr/bin/starry-known-fail)
