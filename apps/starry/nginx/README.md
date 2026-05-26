# Starry Nginx App

This app runs Nginx integration tests inside StarryOS QEMU.

## Run Smoke

```bash
cargo xtask starry app run -t nginx --arch x86_64
```

## Build/Prepare Logic

`prebuild.sh` injects reusable nginx test scripts into the app overlay.

Package installation happens in the guest prepare stage before test execution:

- packages: `nginx`, `curl`, `busybox-extras`, `coreutils`
- mirror priority: domestic mirror first, then official global mirror
- each mirror attempt uses timeout (`NGINX_APK_MIRROR_TIMEOUT_SEC`, default `45`)
- mirror failure triggers automatic fallback
- if all mirrors fail, tests fail early at prepare stage and do not enter nginx
  functional checks

The shared helper logic is in:

- `apps/starry/nginx/nginx-alpine-mirror.sh`
