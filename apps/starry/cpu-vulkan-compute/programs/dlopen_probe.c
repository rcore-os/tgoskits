// dlopen_probe.c - decisive on-target check that runtime dlopen works on StarryOS for a DYNAMIC
// (non-static) musl binary. The static-musl carpets hit musl's static dlopen stub ("Dynamic loading
// not supported"); a dynamic binary routes dlopen through the real ld-musl. This probe confirms the
// path the Python (pyVulkan/cffi) and other dlopen-based bindings depend on. Not a carpet - a
// diagnostic run_all prints without gating on.
#include <dlfcn.h>
#include <stdio.h>

int main(void) {
    void *h = dlopen("libvulkan.so.1", RTLD_NOW | RTLD_GLOBAL);
    if (!h) {
        printf("DLOPEN_PROBE FAIL: dlopen(libvulkan.so.1): %s\n", dlerror());
        return 1;
    }
    void *sym = dlsym(h, "vkCreateInstance");
    if (!sym) {
        printf("DLOPEN_PROBE FAIL: dlsym(vkCreateInstance): %s\n", dlerror());
        dlclose(h);
        return 1;
    }
    printf("DLOPEN_PROBE OK: dlopen+dlsym of libvulkan works on this dynamic binary\n");
    dlclose(h);
    return 0;
}
