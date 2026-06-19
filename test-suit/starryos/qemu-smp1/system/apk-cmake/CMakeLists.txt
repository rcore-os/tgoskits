cmake_minimum_required(VERSION 3.20)

project(apk_cmake NONE)

install(
    PROGRAMS src/apk-cmake.sh
    DESTINATION usr/bin/starry-test-suit
    RENAME apk-cmake
)
