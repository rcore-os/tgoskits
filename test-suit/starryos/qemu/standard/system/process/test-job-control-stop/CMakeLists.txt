cmake_minimum_required(VERSION 3.20)
project(test-job-control-stop C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-job-control-stop src/main.c)
target_include_directories(test-job-control-stop PRIVATE src)
target_compile_options(test-job-control-stop PRIVATE -Wall -Wextra -Werror)
target_link_libraries(test-job-control-stop PRIVATE pthread)

install(TARGETS test-job-control-stop RUNTIME DESTINATION usr/bin/starry-test-suit)
