cmake_minimum_required(VERSION 3.20)
project(perf-hw-group C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
# Opens a {cycles,instructions} event group, enables only the leader (the member
# must start via group propagation), and reads the leader with PERF_FORMAT_GROUP
# to check the group layout + id demux. Hardware PMU (ARM PMUv3); main() skips as
# a pass off aarch64.
add_executable(perf-hw-group src/perf_hw_group.c)
target_compile_options(perf-hw-group PRIVATE -Wall -Wextra -Werror -O0)
install(TARGETS perf-hw-group RUNTIME DESTINATION usr/bin/starry-test-suit)
