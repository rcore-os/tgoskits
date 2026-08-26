#include "wakeup-latency-bench.h"

#include <stdint.h>

int main(void)
{
    uint64_t first = bench_monotonic_ns();
    uint64_t second = bench_monotonic_ns();
    return second >= first ? 0 : 1;
}
