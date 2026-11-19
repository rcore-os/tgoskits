if [ "${AXBUILD_TEST_DUP2_AUTORUN_DONE:-0}" = "1" ]; then
    return 0 2>/dev/null || exit 0
fi

export AXBUILD_TEST_DUP2_AUTORUN_DONE=1

if [ -x /usr/bin/test-dup2 ]; then
    /usr/bin/test-dup2
fi
