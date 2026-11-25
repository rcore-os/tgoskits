#!/bin/sh

if [ "${IVC_RUN_BENCH:-0}" = "1" ]; then
	exec /usr/bin/ivc-starry-bench
fi

exec /usr/bin/ivc-demo
