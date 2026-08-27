if grep -q 'starry.interactive=picoclaw' /proc/cmdline 2>/dev/null; then
    export HOME=/root
    export USER=root
    export SHELL=/bin/sh
    export TERM=xterm-256color
    export NO_COLOR=1
    export PATH=/usr/local/bin:/usr/bin:/bin:/sbin
    export PICOCLAW_HOME=/root/.picoclaw
    export PICOCLAW_CONFIG=/root/.picoclaw/config.json
    export SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
    mkdir -p "$PICOCLAW_HOME" "$PICOCLAW_HOME/workspace"
    if [ -f "$PICOCLAW_HOME/starry-online-env" ]; then
        . "$PICOCLAW_HOME/starry-online-env"
    fi
    cd "$PICOCLAW_HOME/workspace"
    cat <<'STARRY_PICOCLAW_USAGE'

StarryOS PicoClaw interactive shell is ready.

常用命令:
  picoclaw status
  picoclaw agent
  picoclaw agent -m '你好，请用一句话介绍你自己'
  picoclaw gateway --allow-empty --host 127.0.0.1 --port 18790

宿主机访问 gateway:
  curl http://127.0.0.1:18790/health

退出 QEMU:
  Ctrl-a x

STARRY_PICOCLAW_USAGE
fi
