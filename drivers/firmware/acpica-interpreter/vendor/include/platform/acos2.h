/******************************************************************************
 *
 * Name: acos2.h - OS/2 specific defines, etc.
 *
 *****************************************************************************/

/******************************************************************************
 *
 * Copyright (c) 1999 - 2026, Intel Corp.
 * SPDX-License-Identifier: BSD-3-Clause OR GPL-2.0-only
 *
 *****************************************************************************/

#ifndef __ACOS2_H__
#define __ACOS2_H__

#define ACPI_USE_STANDARD_HEADERS
#define ACPI_USE_SYSTEM_CLIBRARY

#define INCL_LONGLONG
#include <os2.h>


#define ACPI_MACHINE_WIDTH          32

#define COMPILER_DEPENDENT_INT64    long long
#define COMPILER_DEPENDENT_UINT64   unsigned long long
#define ACPI_USE_NATIVE_DIVIDE
#define ACPI_USE_NATIVE_MATH64

#define ACPI_SYSTEM_XFACE           APIENTRY
#define ACPI_EXTERNAL_XFACE         APIENTRY
#define ACPI_INTERNAL_XFACE         APIENTRY
#define ACPI_INTERNAL_VAR_XFACE     APIENTRY

/*
 * Some compilers complain about unused variables. Sometimes we don't want to
 * use all the variables (most specifically for _THIS_MODULE). This allow us
 * to to tell the compiler warning in a per-variable manner that a variable
 * is unused.
 */
#define ACPI_UNUSED_VAR

#include <io.h>

#define ACPI_FLUSH_CPU_CACHE() Wbinvd()
void Wbinvd(void);

#define ACPI_ACQUIRE_GLOBAL_LOCK(GLptr, Acq)       Acq = OSPMAcquireGlobalLock(GLptr)
#define ACPI_RELEASE_GLOBAL_LOCK(GLptr, Pnd)       Pnd = OSPMReleaseGlobalLock(GLptr)
unsigned short OSPMAcquireGlobalLock (void *);
unsigned short OSPMReleaseGlobalLock (void *);

#define ACPI_SHIFT_RIGHT_64(n_hi, n_lo) \
{ \
    unsigned long long val = 0LL; \
    val = n_lo | ( ((unsigned long long)h_hi) << 32 ); \
    __llrotr (val,1); \
    n_hi = (unsigned long)((val >> 32 ) & 0xffffffff ); \
    n_lo = (unsigned long)(val & 0xffffffff); \
}

#ifndef ACPI_ASL_COMPILER
#define ACPI_USE_LOCAL_CACHE
#undef ACPI_DEBUGGER
#endif

#endif /* __ACOS2_H__ */
