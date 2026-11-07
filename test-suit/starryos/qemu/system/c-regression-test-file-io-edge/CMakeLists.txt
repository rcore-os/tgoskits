cmake_minimum_required(VERSION 3.20)
project(test-file-io-edge C)
set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS OFF)
add_executable(test-file-io-edge src/main.c)
target_include_directories(test-file-io-edge PRIVATE src)
target_compile_options(test-file-io-edge PRIVATE -Wall -Wextra -Werror)
install(TARGETS test-file-io-edge RUNTIME DESTINATION usr/bin/starry-test-suit)

set(FILE_IO_EDGE_FIXTURE_DIR "${CMAKE_CURRENT_BINARY_DIR}/file-io-edge")
set(FILE_IO_EDGE_FIXTURE
    "${FILE_IO_EDGE_FIXTURE_DIR}/readahead-cold"
)
file(MAKE_DIRECTORY "${FILE_IO_EDGE_FIXTURE_DIR}")
string(REPEAT "R" 4096 FILE_IO_EDGE_PAGE)
string(REPEAT "${FILE_IO_EDGE_PAGE}" 40 FILE_IO_EDGE_BODY)
string(REPEAT "T" 123 FILE_IO_EDGE_TAIL)
file(WRITE "${FILE_IO_EDGE_FIXTURE}" "${FILE_IO_EDGE_BODY}${FILE_IO_EDGE_TAIL}")

# Keep the fixture outside usr/bin so the grouped runner discovers only test
# executables. Explicit permissions prevent accidental installation as a test.
install(
    FILES "${FILE_IO_EDGE_FIXTURE}"
    DESTINATION usr/share/starry-test-suit/file-io-edge
    PERMISSIONS OWNER_READ OWNER_WRITE GROUP_READ WORLD_READ
)
