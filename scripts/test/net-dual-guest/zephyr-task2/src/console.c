#include "console.h"

#include <stdarg.h>

#include <zephyr/kernel.h>
#include <zephyr/sys/atomic.h>
#include <zephyr/sys/printk.h>

#ifndef TASK2_RUNTIME_TRACE
#define TASK2_RUNTIME_TRACE 0
#endif

K_MUTEX_DEFINE(task2_console_mutex);
static atomic_t trace_quiet;

void task2_console_lock(void)
{
	(void)k_mutex_lock(&task2_console_mutex, K_FOREVER);
}

void task2_console_unlock(void)
{
	(void)k_mutex_unlock(&task2_console_mutex);
}

void task2_console_printf_locked(const char *format, ...)
{
	va_list arguments;

	va_start(arguments, format);
	vprintk(format, arguments);
	va_end(arguments);
}

void task2_console_printf(const char *format, ...)
{
	va_list arguments;

	task2_console_lock();
	va_start(arguments, format);
	vprintk(format, arguments);
	va_end(arguments);
	task2_console_unlock();
}

void task2_console_set_trace_quiet(bool quiet)
{
	atomic_set(&trace_quiet, quiet ? 1 : 0);
	/* Drain a trace call which observed the old state before returning. */
	task2_console_lock();
	task2_console_unlock();
}

void task2_console_trace_printf(const char *format, ...)
{
#if TASK2_RUNTIME_TRACE
	va_list arguments;

	if (atomic_get(&trace_quiet) != 0) {
		return;
	}
	task2_console_lock();
	/* Recheck after locking so the quiet transition cannot race this call. */
	if (atomic_get(&trace_quiet) == 0) {
		va_start(arguments, format);
		vprintk(format, arguments);
		va_end(arguments);
	}
	task2_console_unlock();
#else
	ARG_UNUSED(format);
#endif
}
