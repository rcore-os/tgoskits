cmake_minimum_required(VERSION 3.20)
project(nix_smoke_prebuild NONE)

install(PROGRAMS nix-smoke DESTINATION usr/bin)

install(CODE "
set(dest \"\$ENV{DESTDIR}\")
file(MAKE_DIRECTORY \"\${dest}/usr/bin\" \"\${dest}/usr/lib\" \"\${dest}/etc\")
execute_process(
    COMMAND sh -c \"for path in '${STARRY_STAGING_ROOT}/usr/bin/nix'*; do cp -aL \\\"\$path\\\" \\\"\$1/usr/bin/\\\"; done; cp -aL '${STARRY_STAGING_ROOT}/usr/lib/'*.so* \\\"\$1/usr/lib/\\\"; cp -aL '${STARRY_STAGING_ROOT}/etc/nix' \\\"\$1/etc/\\\"\"
    sh \"\${dest}\"
    COMMAND_ERROR_IS_FATAL ANY
)
")
