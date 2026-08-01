/* SPDX-License-Identifier: Apache-2.0 */

#ifndef RT_BASELINE_MISS_ACCOUNTING_H_
#define RT_BASELINE_MISS_ACCOUNTING_H_

#include <stdint.h>

struct coalesced_expirations {
	uint32_t warmup;
	uint32_t measured;
};

/*
 * A binary semaphore represents the first unprocessed expiration. Any later
 * expiration already available at that observation was coalesced. Partition
 * those later expiration indexes at the warm-up/measurement boundary.
 */
static inline struct coalesced_expirations count_coalesced_expirations(
	uint32_t processed_expirations, uint32_t available_expirations,
	uint32_t warmup_count)
{
	const uint32_t first_coalesced = processed_expirations + 1U;
	const uint32_t warmup_end = available_expirations < warmup_count
					? available_expirations
					: warmup_count;
	const uint32_t measured_start = first_coalesced > warmup_count
					  ? first_coalesced
					  : warmup_count;

	return (struct coalesced_expirations){
		.warmup = warmup_end > first_coalesced
				  ? warmup_end - first_coalesced
				  : 0U,
		.measured = available_expirations > measured_start
				    ? available_expirations - measured_start
				    : 0U,
	};
}

#endif /* RT_BASELINE_MISS_ACCOUNTING_H_ */
