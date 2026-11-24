#include <stdio.h>

#include <libuvc/libuvc.h>

int main(void) {
    uvc_context_t *context = NULL;
    const uvc_error_t result = uvc_init(&context, NULL);

    if (result != UVC_SUCCESS) {
        printf("STARRY_SG2002_LIBUVC_INIT_FAILED: uvc_init=%d (%s)\n",
               result, uvc_strerror(result));
        return 1;
    }

    uvc_exit(context);
    puts("STARRY_SG2002_LIBUVC_INIT_OK");
    return 0;
}
