cmake_minimum_required(VERSION 3.20)
project(bugfix-pidfd-fdinfo C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-pidfd-fdinfo src/main.c)
target_compile_options(bugfix-pidfd-fdinfo PRIVATE -Wall -Wextra -Werror)
target_link_libraries(bugfix-pidfd-fdinfo PRIVATE pthread)
install(TARGETS bugfix-pidfd-fdinfo RUNTIME DESTINATION usr/bin/starry-test-suit)
