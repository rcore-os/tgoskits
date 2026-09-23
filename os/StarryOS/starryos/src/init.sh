#!/bin/sh

export HOME=/root
export USER=root
export HOSTNAME=starry
export TERM=xterm-256color
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

printf "Welcome to \033[96m\033[1mStarry OS\033[0m!\n"
echo STARRY_LEGACY_BOARD_INIT
env
echo

if [ -f /usr/bin/starry-run-case-tests ]; then
    echo "STARRY_GROUPED_AUTORUN_INIT"
    export AXBUILD_GROUPED_AUTORUN_DONE=1
    sh /usr/bin/starry-run-case-tests
fi

# The persistent board images do not run udevd. Populate the known device
# records so libudev recognizes the input and display nodes.
mkdir /run 2>/dev/null
mkdir /run/udev 2>/dev/null
mkdir /run/udev/data 2>/dev/null
touch /run/udev/data/c226:0 2>/dev/null || true
touch /run/udev/data/c29:0 2>/dev/null || true
for i in 0 1 2 3 4 5 6 7; do
    touch "/run/udev/data/c13:$((64 + i))" 2>/dev/null || true
done

# Keep the existing visual test hook on boards using this init mode.
if [ -x /test_runner.sh ]; then
    echo "[init] /test_runner.sh detected, launching visual scenario"
    setsid /test_runner.sh </dev/null >/dev/console 2>&1 &
    echo "[init] /test_runner.sh started pid=$!"
fi

cd "$HOME" || cd /

cat > /tmp/starry-shrc <<'EOF'
starry_prompt_dir() {
    case "$PWD" in
        "$HOME") printf '~' ;;
        "$HOME"/*) printf '~%s' "${PWD#"$HOME"}" ;;
        *) printf '%s' "$PWD" ;;
    esac
}
export PS1='${USER}@${HOSTNAME}:$(starry_prompt_dir)# '
EOF
export ENV=/tmp/starry-shrc
exec /bin/sh -l -i
