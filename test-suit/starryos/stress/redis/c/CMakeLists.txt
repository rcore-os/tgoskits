cmake_minimum_required(VERSION 3.20)
project(redis-stress-tests NONE)

install(PROGRAMS
    ${STARRY_STAGING_ROOT}/usr/bin/redis-server
    ${STARRY_STAGING_ROOT}/usr/bin/redis-cli
    DESTINATION usr/bin
)

file(GLOB REDIS_LIBSTDCXX "${STARRY_STAGING_ROOT}/usr/lib/libstdc++.so.6.*")
list(GET REDIS_LIBSTDCXX 0 REDIS_LIBSTDCXX_FILE)

install(FILES
    ${STARRY_STAGING_ROOT}/usr/lib/libssl.so.3
    ${STARRY_STAGING_ROOT}/usr/lib/libcrypto.so.3
    ${STARRY_STAGING_ROOT}/usr/lib/libgcc_s.so.1
    DESTINATION usr/lib
)
install(FILES ${REDIS_LIBSTDCXX_FILE}
    RENAME libstdc++.so.6
    DESTINATION usr/lib
)
install(PROGRAMS src/redis-stress-tests.sh DESTINATION usr/bin)
