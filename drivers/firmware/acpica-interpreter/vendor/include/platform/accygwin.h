/******************************************************************************
 *
 * Name: accygwin.h - OS specific defines, etc. for cygwin environment
 *
 *****************************************************************************/

/******************************************************************************
 *
 * Copyright (c) 1999 - 2026, Intel Corp.
 * SPDX-License-Identifier: BSD-3-Clause OR GPL-2.0-only
 *
 *****************************************************************************/

#ifndef __ACCYGWIN_H__
#define __ACCYGWIN_H__

/*
 * ACPICA configuration
 */
#define ACPI_USE_STANDARD_HEADERS
#define ACPI_USE_SYSTEM_CLIBRARY
#define ACPI_USE_DO_WHILE_0
#define ACPI_FLUSH_CPU_CACHE()

/*
 * This is needed since sem_timedwait does not appear to work properly
 * on cygwin (always hangs forever).
 */
#define ACPI_USE_ALTERNATE_TIMEOUT


#ifdef ACPI_USE_STANDARD_HEADERS
#include <unistd.h>
#endif

#if defined(__ia64__) || defined(__x86_64__)
#define ACPI_MACHINE_WIDTH          64
#define COMPILER_DEPENDENT_INT64    long
#define COMPILER_DEPENDENT_UINT64   unsigned long
#else
#define ACPI_MACHINE_WIDTH          32
#define COMPILER_DEPENDENT_INT64    long long
#define COMPILER_DEPENDENT_UINT64   unsigned long long
#define ACPI_USE_NATIVE_DIVIDE
#define ACPI_USE_NATIVE_MATH64
#endif

#ifndef __cdecl
#define __cdecl
#endif

#define ACPI_ACQUIRE_GLOBAL_LOCK(GLptr, Acq) if (GLptr) Acq=1; else Acq=0;
#define ACPI_RELEASE_GLOBAL_LOCK(GLptr, Pending) Pending = 1

/* On Cygwin, pthread_t is a pointer */

#define ACPI_CAST_PTHREAD_T(pthread) ((ACPI_THREAD_ID) ACPI_TO_INTEGER (pthread))


/*
 * The vsnprintf/snprintf functions are defined by c99, but cygwin/gcc
 * does not enable this prototype when the -ansi flag is set. Also related
 * to __STRICT_ANSI__. So, we just declare the prototype here.
 */
int
vsnprintf (char *s, size_t n, const char *format, va_list ap);

int
snprintf (char *s, size_t n, const char *format, ...);

#endif /* __ACCYGWIN_H__ */
