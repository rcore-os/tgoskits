/*
 * Human-readable diagnostics for the adapter error codes.
 *
 * Smoke programs print these strings on their failure lines so a guest log
 * distinguishes "device absent" (NOT_FOUND), "mapping failed" (MMAP), and
 * "event never arrived" (TIMEOUT) without a debugger.
 */
#include "ivshmem.h"

const char *ivshmem_strerror(int err)
{
    switch (err) {
    case IVSHMEM_OK:
        return "ok";
    case IVSHMEM_ERR_NOT_FOUND:
        return "no matching ivshmem device found";
    case IVSHMEM_ERR_AMBIGUOUS:
        return "several ivshmem devices found; an explicit BDF is required";
    case IVSHMEM_ERR_PROFILE:
        return "device identity or revision does not match the adapter profile";
    case IVSHMEM_ERR_IO:
        return "sysfs or devtmpfs access failed";
    case IVSHMEM_ERR_MMAP:
        return "BAR mapping failed";
    case IVSHMEM_ERR_BACKEND:
        return "requested event backend is unavailable";
    case IVSHMEM_ERR_TIMEOUT:
        return "wait timed out";
    case IVSHMEM_ERR_NOMEM:
        return "out of memory";
    case IVSHMEM_ERR_ARGS:
        return "invalid argument";
    default:
        return "unknown ivshmem error";
    }
}
