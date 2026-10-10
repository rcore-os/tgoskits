/* TGOSKits ACPICA platform configuration; Apache-2.0. */
#ifndef ACTGOSKITS_H
#define ACTGOSKITS_H

#if !defined(__x86_64__) || !defined(__LP64__)
#error "acpica-interpreter currently requires the x86_64 LP64 ABI"
#endif

#define ACPI_MACHINE_WIDTH 64
#define ACPI_USE_LOCAL_CACHE
#define ACPI_USE_BUILTIN_STDARG
#define ACPI_USE_NATIVE_DIVIDE
#define ACPI_USE_NATIVE_MATH64
#define ACPI_MUTEX_TYPE ACPI_BINARY_SEMAPHORE
#define ACPI_SPINLOCK void *
#define ACPI_CPU_FLAGS unsigned long
#define ACPI_USE_NATIVE_RSDP_POINTER

/* Match the ACPICA Linux x86 configuration; native sleep still needs validation. */
#define ACPI_FLUSH_CPU_CACHE()

/* Implement the ACPI FACS global lock's ownership and pending bits atomically. */
#define ACPI_ACQUIRE_GLOBAL_LOCK(p, a) do { \
    unsigned int old = __atomic_load_n(&((p)->GlobalLock), __ATOMIC_RELAXED), next; \
    do { \
        next = (old & ~3u) | 2u | ((old & 2u) >> 1); \
    } while (!__atomic_compare_exchange_n(&((p)->GlobalLock), &old, next, 0, \
        __ATOMIC_ACQ_REL, __ATOMIC_RELAXED)); \
    (a) = !(next & 1u); \
} while (0)
#define ACPI_RELEASE_GLOBAL_LOCK(p, a) do { \
    unsigned int old = __atomic_fetch_and(&((p)->GlobalLock), ~3u, __ATOMIC_RELEASE); \
    (a) = old & 1u; \
} while (0)

#endif
