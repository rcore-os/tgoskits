#!/bin/sh

export HOME=/root
export USER=root
export HOSTNAME=starry
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

printf "Welcome to \033[96m\033[1mStarry OS\033[0m!\n"
env
echo

printf "Use \033[1m\033[3mapk\033[0m to install packages.\n"
echo

# Do your initialization here!

cd "$HOME" || cd /
cat > /tmp/starry-shrc <<'EOF'
export PS1='${USER}@${HOSTNAME}:${PWD} # '
EOF
export ENV=/tmp/starry-shrc
exec /bin/sh -l -i
