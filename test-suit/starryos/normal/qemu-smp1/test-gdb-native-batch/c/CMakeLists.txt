cmake_minimum_required(VERSION 3.20)
project(test-gdb-native-batch C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)

add_executable(test-gdb-native-batch-target src/main.c)
target_compile_options(test-gdb-native-batch-target PRIVATE -Wall -Wextra -Werror -O0 -g)

install(TARGETS test-gdb-native-batch-target RUNTIME DESTINATION usr/bin)
install(FILES gdb-native-batch.gdb DESTINATION usr/bin)
install(PROGRAMS run-gdb-native-batch.sh DESTINATION usr/bin)
