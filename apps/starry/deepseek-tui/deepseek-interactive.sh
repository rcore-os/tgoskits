if grep -q 'starry.interactive=deepseek' /proc/cmdline 2>/dev/null; then
    export HOME=/root
    export USER=root
    export SHELL=/bin/sh
    export TERM=xterm-256color
    export PATH=/usr/local/bin:/usr/bin:/bin:/sbin
    export SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt
    if [ -f "$HOME/.deepseek/starry-online-env" ]; then
        . "$HOME/.deepseek/starry-online-env"
    fi
    cd "$HOME"
    cat <<'STARRY_DEEPSEEK_USAGE'

StarryOS DeepSeek TUI interactive shell is ready.

Common commands:
  deepseek --version
  deepseek-tui --version
  deepseek model list
  deepseek-tui

Exit QEMU:
  Ctrl-a x

STARRY_DEEPSEEK_USAGE
fi
