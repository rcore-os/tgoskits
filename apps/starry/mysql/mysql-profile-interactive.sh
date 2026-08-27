if grep -q 'starry.interactive=mysql' /proc/cmdline 2>/dev/null; then
    exec /usr/bin/mysql-interactive.sh
fi
