cmake_minimum_required(VERSION 3.20)
project(bugfix-ext4-symlink-60-byte-target C)

set(CMAKE_C_STANDARD 11)
set(CMAKE_C_STANDARD_REQUIRED ON)
set(CMAKE_C_EXTENSIONS ON)

add_executable(bugfix-ext4-symlink-60-byte-target src/main.c)
target_compile_options(
    bugfix-ext4-symlink-60-byte-target
    PRIVATE -Wall -Wextra -Werror
)
install(
    TARGETS bugfix-ext4-symlink-60-byte-target
    RUNTIME DESTINATION usr/bin/starry-test-suit
)

set(STARRY_SYMLINK_FIXTURE_DIR
    "usr/share/starry-test-suit/ext4-symlink-60-byte-target"
)
string(REPEAT "a" 58 STARRY_SYMLINK_TARGET_59_SUFFIX)
string(REPEAT "b" 59 STARRY_SYMLINK_TARGET_60_SUFFIX)
string(REPEAT "c" 60 STARRY_SYMLINK_TARGET_61_SUFFIX)
set(STARRY_SYMLINK_TARGET_59 "/${STARRY_SYMLINK_TARGET_59_SUFFIX}")
set(STARRY_SYMLINK_TARGET_60 "/${STARRY_SYMLINK_TARGET_60_SUFFIX}")
set(STARRY_SYMLINK_TARGET_61 "/${STARRY_SYMLINK_TARGET_61_SUFFIX}")

# Create these links in the host overlay rather than in the guest. The ext4
# image injector therefore chooses the on-disk fast/regular symlink encoding.
install(CODE "
    file(MAKE_DIRECTORY
        \"\$ENV{DESTDIR}/${STARRY_SYMLINK_FIXTURE_DIR}\"
    )
    file(CREATE_LINK
        \"${STARRY_SYMLINK_TARGET_59}\"
        \"\$ENV{DESTDIR}/${STARRY_SYMLINK_FIXTURE_DIR}/link-59\"
        SYMBOLIC
    )
    file(CREATE_LINK
        \"${STARRY_SYMLINK_TARGET_60}\"
        \"\$ENV{DESTDIR}/${STARRY_SYMLINK_FIXTURE_DIR}/link-60\"
        SYMBOLIC
    )
    file(CREATE_LINK
        \"${STARRY_SYMLINK_TARGET_61}\"
        \"\$ENV{DESTDIR}/${STARRY_SYMLINK_FIXTURE_DIR}/link-61\"
        SYMBOLIC
    )
")
