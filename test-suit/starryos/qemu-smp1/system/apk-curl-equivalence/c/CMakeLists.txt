cmake_minimum_required(VERSION 3.20)
project(apk-curl-equivalence NONE)

if(NOT DEFINED STARRY_STAGING_ROOT)
    message(FATAL_ERROR "STARRY_STAGING_ROOT must be provided by axbuild")
endif()

set(CURL_BIN "${STARRY_STAGING_ROOT}/usr/bin/curl")

if(NOT EXISTS "${CURL_BIN}")
    message(FATAL_ERROR "curl is missing from ${STARRY_STAGING_ROOT}/usr/bin")
endif()

install(PROGRAMS "${CURL_BIN}" DESTINATION usr/bin)
install(PROGRAMS src/apk-curl-equivalence.sh
    DESTINATION usr/bin/starry-test-suit
    RENAME apk-curl-equivalence
)
