#!/bin/sh
set -eu

# The Python asset pipeline installs python3 before running this script. Keep
# the server and interpreter in the same staging root so the pipeline carries
# both into the overlay.
apk add memcached
test -x "$STARRY_STAGING_ROOT/usr/bin/python3"
test -x "$STARRY_STAGING_ROOT/usr/bin/memcached"
