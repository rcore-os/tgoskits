cmake_minimum_required(VERSION 3.20)
project(bug-redis-aof-appendonly C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)
add_executable(bug-redis-aof-appendonly src/main.c)
target_compile_options(bug-redis-aof-appendonly PRIVATE -Wall -Wextra -Werror)
# This Redis/AOF probe requires the Redis app rootfs setup; keep it out of
# the generic system grouped runner.
install(TARGETS bug-redis-aof-appendonly RUNTIME DESTINATION usr/bin/starry-known-fail)
