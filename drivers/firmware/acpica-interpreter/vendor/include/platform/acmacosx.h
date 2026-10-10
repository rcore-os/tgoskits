/******************************************************************************
 *
 * Name: acmacosx.h - OS specific defines, etc. for Mac OS X
 *
 *****************************************************************************/

/******************************************************************************
 *
 * Copyright (c) 1999 - 2026, Intel Corp.
 * SPDX-License-Identifier: BSD-3-Clause OR GPL-2.0-only
 *
 *****************************************************************************/

#ifndef __ACMACOSX_H__
#define __ACMACOSX_H__

#include "aclinux.h"

#ifdef __APPLE__
#define ACPI_USE_ALTERNATE_TIMEOUT
#endif /* __APPLE__ */

#ifdef __clang__
#pragma clang diagnostic ignored "-Wformat-nonliteral"
#endif

#endif /* __ACMACOSX_H__ */
