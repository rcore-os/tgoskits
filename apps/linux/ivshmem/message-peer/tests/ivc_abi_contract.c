/*
 * Public Linux C ABI contract for the message-peer static library.
 *
 * Compile with the repository-owned common header, for example:
 * cc -std=c11 -Wall -Wextra -Werror \
 *   -Iapps/common/ivc/include tests/ivc_abi_contract.c \
 *   target/<target>/release/liblinux_ivc.a -o ivc-abi-contract
 */
#include <errno.h>
#include <stdint.h>
#include <stdio.h>

#include <ivc.h>

_Static_assert(IVC_MAX_RECORD_BYTES == 48U, "record limit changed");
_Static_assert(IVC_MAX_TIMEOUT_MS == 60000, "timeout limit changed");

typedef int (*open_fn)(struct ivc **);
typedef int (*send_fn)(struct ivc *, const void *, size_t, int32_t);
typedef int (*recv_fn)(struct ivc *, void *, size_t, int32_t);
typedef int (*close_fn)(struct ivc *);

static const open_fn exported_open = ivc_open;
static const send_fn exported_send = ivc_send;
static const recv_fn exported_recv = ivc_recv;
static const close_fn exported_close = ivc_close;

static int expect(int actual, int expected, const char *operation)
{
    if (actual == expected)
        return 0;

    fprintf(stderr, "%s returned %d, expected %d\n", operation, actual,
            expected);
    return 1;
}

int main(void)
{
    struct ivc *handle = NULL;
    unsigned char byte = 0;
    int failed = 0;

    (void)exported_open;
    (void)exported_send;
    (void)exported_recv;
    (void)exported_close;

    failed |= expect(ivc_open(NULL), -EINVAL, "ivc_open(NULL)");
    failed |= expect(ivc_send(NULL, NULL, 0, 0), -EINVAL,
                     "ivc_send(NULL, NULL, 0, 0)");
    failed |= expect(ivc_recv(NULL, NULL, 0, 0), -EINVAL,
                     "ivc_recv(NULL, NULL, 0, 0)");
    failed |= expect(ivc_recv(NULL, &byte, 0, 0), -EINVAL,
                     "ivc_recv(..., cap=0)");
    failed |= expect(ivc_send(NULL, &byte, 1, -1), -EINVAL,
                     "ivc_send(..., timeout=-1)");
    failed |= expect(ivc_recv(NULL, &byte, 1, IVC_MAX_TIMEOUT_MS + 1),
                     -EINVAL, "ivc_recv(..., timeout>max)");
    failed |= expect(ivc_close(NULL), -EINVAL, "ivc_close(NULL)");

    handle = (struct ivc *)(uintptr_t)1;
    {
        int result = ivc_open(&handle);
        if (result == 0) {
            failed |= expect(handle != NULL, 1, "ivc_open success handle");
            failed |= expect(ivc_close(handle), 0, "ivc_close(open handle)");
        } else {
            failed |= expect(result < 0, 1, "ivc_open failure errno");
            failed |= expect(handle == NULL, 1, "ivc_open failure clears out");
        }
    }

    if (failed)
        return 1;

    puts("ivc C ABI contract pass");
    return 0;
}
