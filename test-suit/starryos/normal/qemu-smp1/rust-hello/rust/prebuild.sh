#!/bin/sh
# Installs zlib-dev so the rust-hello binary can link against zlib.
# This validates the Rust case prebuild.sh pipeline end-to-end.
set -eu
apk add --no-cache zlib-dev
