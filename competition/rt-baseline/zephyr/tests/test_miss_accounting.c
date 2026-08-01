/* SPDX-License-Identifier: Apache-2.0 */

#include <assert.h>
#include <stdint.h>

#include "../src/miss_accounting.h"

#define WARMUP_COUNT 100U
#define SAMPLE_COUNT 10000U
#define TOTAL_EXPIRATIONS (WARMUP_COUNT + SAMPLE_COUNT)

static void expect_partition(uint32_t processed_expirations,
			     uint32_t available_expirations,
			     uint32_t expected_warmup,
			     uint32_t expected_measured)
{
	const struct coalesced_expirations actual =
		count_coalesced_expirations(processed_expirations,
					      available_expirations,
					      WARMUP_COUNT);

	assert(actual.warmup == expected_warmup);
	assert(actual.measured == expected_measured);
}

int main(void)
{
	expect_partition(0U, 1U, 0U, 0U);
	expect_partition(0U, 3U, 2U, 0U);
	expect_partition(98U, 102U, 1U, 2U);
	expect_partition(99U, 101U, 0U, 1U);
	expect_partition(100U, 103U, 0U, 2U);
	expect_partition(TOTAL_EXPIRATIONS - 1U, TOTAL_EXPIRATIONS, 0U, 0U);
	expect_partition(0U, TOTAL_EXPIRATIONS, WARMUP_COUNT - 1U,
			 SAMPLE_COUNT);
	return 0;
}
