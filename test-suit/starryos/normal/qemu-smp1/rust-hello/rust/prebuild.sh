#!/bin/sh
# Writes a marker file that main.rs embeds at compile time via include_str!.
# This validates the prebuild.sh pipeline end-to-end without needing network access.
set -eu
echo "prebuild-ok" > src/prebuild_marker.txt
