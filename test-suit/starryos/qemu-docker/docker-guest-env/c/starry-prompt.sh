# The Debian container-host image boots through the unified init chain:
# BusyBox /sbin/init respawns a console login shell (dash). This profile hook
# gives that shell the conventional root@starry prompt the Starry QEMU cases
# wait for. dash prints PS1 literally, so keep it static.
PS1='root@starry:~# '
export PS1
