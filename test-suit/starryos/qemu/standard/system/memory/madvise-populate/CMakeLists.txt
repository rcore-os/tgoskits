cmake_minimum_required(VERSION 3.20)
project(madvise-populate C)

# Linux kselftest is the primary ABI test; keep only non-overlapping supplements.
add_executable(madvise-populate upstream/madv_populate.c)
add_executable(madvise-populate-supplement src/file-and-prefix.c)
foreach(target madvise-populate madvise-populate-supplement)
    target_compile_options(${target} PRIVATE -Wall -Wextra -Werror)
    install(TARGETS ${target} RUNTIME DESTINATION usr/bin/starry-test-suit)
endforeach()
