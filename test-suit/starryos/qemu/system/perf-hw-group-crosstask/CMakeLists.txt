cmake_minimum_required(VERSION 3.20)
project(perf-hw-group-crosstask C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
# Group-leader sampling memory-safety gate: a PERF_FORMAT_GROUP sampling leader
# bakes raw pointers to each counting member's atomics into its per-CPU
# SampleSlot, whose lifetime is bounded to the LEADER's slice. A member on a
# DIFFERENT thread could be freed while the leader's slot still holds its
# pointers (use-after-free in the overflow handler), so the kernel must reject a
# cross-thread group link with EINVAL (mirroring Linux group_leader->ctx). This
# test asserts a same-thread member links OK but a cross-thread member is
# rejected. Hardware PMU (ARM PMUv3); main() skips as a pass off aarch64.
add_executable(perf-hw-group-crosstask src/perf_hw_group_crosstask.c)
target_link_libraries(perf-hw-group-crosstask pthread)
target_compile_options(perf-hw-group-crosstask PRIVATE -Wall -Wextra -Werror)
install(TARGETS perf-hw-group-crosstask RUNTIME DESTINATION usr/bin/starry-test-suit)
