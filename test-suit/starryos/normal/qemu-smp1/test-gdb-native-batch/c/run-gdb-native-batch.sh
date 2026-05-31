#!/bin/sh
set -eu

/usr/bin/gdb -q -batch -x /usr/bin/gdb-native-batch.gdb /usr/bin/test-gdb-native-batch-target
