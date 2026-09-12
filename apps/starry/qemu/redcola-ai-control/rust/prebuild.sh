#!/bin/sh
# Writes a marker file that build.rs embeds at compile time.
set -eu
echo "prebuild-ok" > src/prebuild_marker.txt
