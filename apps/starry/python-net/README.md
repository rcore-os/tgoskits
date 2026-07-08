# python-net — Python web-framework carpet

Industrial-grade, on-target test of a set of Python web / ASGI / GraphQL frameworks, run by
**CPython 3.14** (musl-native) on StarryOS across all four architectures (x86_64 / aarch64 /
riscv64 / loongarch64).

Each module is a self-contained carpet that exercises one framework's public API surface
(hundreds of exact-value assertions). Django is driven through `django.test.Client` against an
in-memory sqlite DB; FastAPI is driven both in-process (raw ASGI) and through a **real uvicorn
ASGI server over IPv4 loopback**; Strawberry executes real GraphQL operations. A module prints
an anchored `*_DONE` marker only when its internal fail count is zero; `run_all.py` runs every
module and emits `TEST PASSED` only when all of them pass (no skip).

## Run

```
cargo xtask starry app qemu -t python-net --arch x86_64
cargo xtask starry app qemu -t python-net --arch aarch64
cargo xtask starry app qemu -t python-net --arch riscv64
cargo xtask starry app qemu -t python-net --arch loongarch64
```

`prebuild.sh` provisions CPython 3.14 + the frameworks by `apk add`-ing them into a base-rootfs
staging tree via qemu-user-static (so each package — including the native `pydantic-core` — is
resolved for the target arch), unzips the arch-independent `strawberry-graphql` wheel into
site-packages, and copies the interpreter, its shared-library closure, the stdlib + site-packages
and the carpet modules into the per-app overlay.

## Coverage

| module | framework | dimension | marker |
|:--|:--|:--|:--|
| django | Django 4.2.30 | routing / converters / views / ORM (sqlite) / templates / forms / middleware / signing / cache via `test.Client` | `DJANGO_DONE` |
| fastapi | FastAPI 0.121.2 + uvicorn 0.38.0 + Pydantic 2.12.3 | routing / path-query-body params + type coercion / 422 validation / deps / middleware / OpenAPI + **real uvicorn ASGI server over loopback** | `FASTAPI_DONE` |
| strawberry | strawberry-graphql 0.316.0 (graphql-core 3.2.6) | schema / types / queries / mutations / enums / interfaces / unions / custom scalars / async resolvers / introspection / errors | `STRAWBERRY_DONE` |

The carpet sources live in `python/`; the frameworks are provisioned by `prebuild.sh` from the
Alpine edge framework apks + the bundled strawberry-graphql wheel in `assets/`.
