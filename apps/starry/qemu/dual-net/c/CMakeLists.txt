cmake_minimum_required(VERSION 3.20)

project(dual-net-test NONE)

install(PROGRAMS dual-net-tests.sh DESTINATION usr/bin)
install(PROGRAMS ${STARRY_STAGING_ROOT}/usr/bin/curl DESTINATION usr/bin)
