/******************************************************************************
 *
 * Name: acdragonflyex.h - Extra OS specific defines, etc. for DragonFly BSD
 *
 *****************************************************************************/

/******************************************************************************
 *
 * Copyright (c) 1999 - 2026, Intel Corp.
 * SPDX-License-Identifier: BSD-3-Clause OR GPL-2.0-only
 *
 *****************************************************************************/

#ifndef __ACDRAGONFLYEX_H__
#define __ACDRAGONFLYEX_H__

#ifdef _KERNEL

#ifdef ACPI_DEBUG_CACHE
ACPI_STATUS
_AcpiOsReleaseObject (
    ACPI_CACHE_T                *Cache,
    void                        *Object,
    const char                  *func,
    int                         line);
#endif

#ifdef ACPI_DEBUG_LOCKS
ACPI_CPU_FLAGS
_AcpiOsAcquireLock (
    ACPI_SPINLOCK               Spin,
    const char                  *func,
    int                         line);
#endif

#ifdef ACPI_DEBUG_MEMMAP
void *
_AcpiOsMapMemory (
    ACPI_PHYSICAL_ADDRESS       Where,
    ACPI_SIZE                   Length,
    const char                  *caller,
    int                         line);

void
_AcpiOsUnmapMemory (
    void                        *LogicalAddress,
    ACPI_SIZE                   Length,
    const char                  *caller,
    int                         line);
#endif

#endif /* _KERNEL */

#endif /* __ACDRAGONFLYEX_H__ */
