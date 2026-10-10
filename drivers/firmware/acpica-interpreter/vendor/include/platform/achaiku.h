/******************************************************************************
 *
 * Name: achaiku.h - OS specific defines, etc. for Haiku (www.haiku-os.org)
 *
 *****************************************************************************/

/******************************************************************************
 *
 * Copyright (c) 1999 - 2026, Intel Corp.
 * SPDX-License-Identifier: BSD-3-Clause OR GPL-2.0-only
 *
 *****************************************************************************/

#ifndef __ACHAIKU_H__
#define __ACHAIKU_H__

#define ACPI_USE_STANDARD_HEADERS
#define ACPI_USE_SYSTEM_CLIBRARY

#include <KernelExport.h>

struct mutex;


/* Host-dependent types and defines for user- and kernel-space ACPICA */

#define ACPI_MUTEX_TYPE             ACPI_OSL_MUTEX
#define ACPI_MUTEX                  struct mutex *

#define ACPI_USE_NATIVE_DIVIDE
#define ACPI_USE_NATIVE_MATH64

/* #define ACPI_THREAD_ID               thread_id */

#define ACPI_SEMAPHORE              sem_id
#define ACPI_SPINLOCK               spinlock *
#define ACPI_CPU_FLAGS              cpu_status

#define COMPILER_DEPENDENT_INT64    int64
#define COMPILER_DEPENDENT_UINT64   uint64


#ifdef B_HAIKU_64_BIT
#define ACPI_MACHINE_WIDTH          64
#else
#define ACPI_MACHINE_WIDTH          32
#endif


#ifdef _KERNEL_MODE
/* Host-dependent types and defines for in-kernel ACPICA */

/* ACPICA cache implementation is adequate. */
#define ACPI_USE_LOCAL_CACHE

/* On other platform the default definition (do nothing) is fine. */
#if defined(__i386__) || defined(__x86_64__)
#define ACPI_FLUSH_CPU_CACHE() __asm __volatile("wbinvd");
#endif

/* Based on FreeBSD's due to lack of documentation */
extern int AcpiOsAcquireGlobalLock(volatile uint32_t *lock);
extern int AcpiOsReleaseGlobalLock(volatile uint32_t *lock);

#define ACPI_ACQUIRE_GLOBAL_LOCK(GLptr, Acq)    do {                \
        (Acq) = AcpiOsAcquireGlobalLock(&((GLptr)->GlobalLock));    \
} while (0)

#define ACPI_RELEASE_GLOBAL_LOCK(GLptr, Acq)    do {                \
        (Acq) = AcpiOsReleaseGlobalLock(&((GLptr)->GlobalLock));    \
} while (0)

#define ACPI_SEMAPHORE_NULL -1

#else /* _KERNEL_MODE */
/* Host-dependent types and defines for user-space ACPICA */

#error "We only support kernel mode ACPI atm."

#endif /* _KERNEL_MODE */
#endif /* __ACHAIKU_H__ */
