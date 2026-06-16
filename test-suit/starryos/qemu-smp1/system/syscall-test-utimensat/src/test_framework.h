#ifndef _TEST_FRAMEWORK_H
#define _TEST_FRAMEWORK_H

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int _test_fail_count = 0;
static const char *_current_test = NULL;

#define TEST_START(name)                                                       \
	do {                                                                   \
		_current_test = name;                                          \
		_test_fail_count = 0;                                          \
		printf("  RUN  %s\n", name);                                   \
	} while (0)

#define TEST_DONE()                                                            \
	do {                                                                   \
		if (_test_fail_count > 0) {                                    \
			printf("  FAIL %s (%d failures)\n", _current_test,     \
			       _test_fail_count);                               \
			exit(1);                                               \
		}                                                              \
		printf("  OK   %s\n", _current_test);                          \
	} while (0)

#define CHECK(cond, msg)                                                       \
	do {                                                                   \
		if (!(cond)) {                                                 \
			fprintf(stderr, "FAIL:%s: %s\n", _current_test, msg);  \
			_test_fail_count++;                                    \
		}                                                              \
	} while (0)

#define CHECK_RET(call, expected, msg)                                         \
	do {                                                                   \
		long _ret = (long)(call);                                      \
		if (_ret != (long)(expected)) {                                \
			fprintf(stderr,                                        \
				"FAIL:%s: %s: expected %ld got %ld (errno=%d:%s)\n", \
				_current_test, msg, (long)(expected), _ret, errno, \
				strerror(errno));                              \
			_test_fail_count++;                                    \
		}                                                              \
	} while (0)

#define CHECK_ERR(call, exp_errno, msg)                                        \
	do {                                                                   \
		long _ret = (long)(call);                                      \
		if (_ret != -1) {                                              \
			fprintf(stderr,                                        \
				"FAIL:%s: %s: expected errno %d but got success (%ld)\n", \
				_current_test, msg, exp_errno, _ret);          \
			_test_fail_count++;                                    \
		} else if (errno != exp_errno) {                               \
			fprintf(stderr,                                        \
				"FAIL:%s: %s: expected errno %d got %d (%s)\n", \
				_current_test, msg, exp_errno, errno,           \
				strerror(errno));                              \
			_test_fail_count++;                                    \
		}                                                              \
	} while (0)

#endif
