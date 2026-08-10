cmake_minimum_required(VERSION 3.20)
project(bugfix-wait-candidate-rescan C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(bugfix-wait-candidate-rescan src/main.c)
target_compile_options(bugfix-wait-candidate-rescan PRIVATE -Wall -Wextra -Werror)
target_link_libraries(bugfix-wait-candidate-rescan PRIVATE pthread)

install(TARGETS bugfix-wait-candidate-rescan RUNTIME DESTINATION usr/bin/starry-test-suit)
