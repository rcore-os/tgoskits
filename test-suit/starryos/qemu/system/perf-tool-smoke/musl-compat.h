#pragma once
/* Force-included via EXTRA_CFLAGS, which also reaches .S files — guard the C
 * content so the assembler never sees it (gcc defines __ASSEMBLER__ for .S). */
#ifndef __ASSEMBLER__
#include <string.h>
/* musl provides only POSIX basename() in <libgen.h> (which may modify its
 * argument); perf uses the GNU string.h basename(). Provide a non-modifying
 * GNU-style shim, force-included via EXTRA_CFLAGS. */
static inline char *__perf_gnu_basename(char *p)
{
    char *s = strrchr(p, '/');
    return s ? s + 1 : p;
}
#define basename __perf_gnu_basename
#endif /* __ASSEMBLER__ */
