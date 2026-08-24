#ifndef TASK2_CONSOLE_H_
#define TASK2_CONSOLE_H_

#include <stdbool.h>

#include <zephyr/toolchain.h>

__printf_like(1, 2) void task2_console_printf(const char *format, ...);
__printf_like(1, 2) void task2_console_trace_printf(const char *format, ...);
void task2_console_set_trace_quiet(bool quiet);
void task2_console_lock(void);
void task2_console_unlock(void);
__printf_like(1, 2) void task2_console_printf_locked(const char *format, ...);

#endif /* TASK2_CONSOLE_H_ */
