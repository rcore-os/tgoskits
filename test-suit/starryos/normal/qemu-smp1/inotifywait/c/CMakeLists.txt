cmake_minimum_required(VERSION 3.20)

project(inotifywait-test NONE)

install(PROGRAMS inotifywait-tests.sh DESTINATION usr/bin)
install(PROGRAMS ${STARRY_STAGING_ROOT}/usr/bin/inotifywait DESTINATION usr/bin)
