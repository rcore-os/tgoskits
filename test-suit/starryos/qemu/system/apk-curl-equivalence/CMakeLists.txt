cmake_minimum_required(VERSION 3.20)
project(apk-curl-equivalence NONE)

if(NOT DEFINED STARRY_STAGING_ROOT)
    message(FATAL_ERROR "STARRY_STAGING_ROOT must be provided by axbuild")
endif()

set(CURL_BIN "${STARRY_STAGING_ROOT}/usr/bin/curl")

if(NOT EXISTS "${CURL_BIN}")
    # Explicit skip, not a silent drop: this subcase verifies the curl
    # installed by prebuild.sh through apk. prebuild.sh runs as a guest
    # process under qemu-user, so hosts without qemu-user (e.g. macOS)
    # cannot stage curl; the runner will not see this program at all.
    message(STATUS
        "apk-curl-equivalence skipped: curl is missing from "
        "${STARRY_STAGING_ROOT}/usr/bin (prebuild.sh cannot run without qemu-user)")
    return()
endif()

install(PROGRAMS "${CURL_BIN}" DESTINATION usr/bin)
install(PROGRAMS src/apk-curl-equivalence.sh
    DESTINATION usr/bin/starry-test-suit
    RENAME apk-curl-equivalence
)
