// void cache_invalidate_d(u64 start, u64 length);
.global cache_invalidate_d
cache_invalidate_d:
	add x2, x0, x1 /* calculate the end address */
	bic x0, x0, #(64 - 1) /* align the start with a cache line */
1:
	dc ivac, x0 /* invalidate cache to PoC by VA */
	add x0, x0, #64
	cmp x0, x2
	blt 1b
	mov x0, xzr
	dsb sy
	ret

// void cache_clean_invalidate_d(u64 start, u64 length);
.global cache_clean_invalidate_d
cache_clean_invalidate_d:
	add x2, x0, x1 /* calculate the end address */
	bic x0, x0, #(64 - 1) /* align the start with a cache line */
1:
	dc civac, x0 /* invalidate cache to PoC by VA */
	add x0, x0, #64
	cmp x0, x2
	blt 1b
	mov x0, xzr
	dsb sy
	ret
