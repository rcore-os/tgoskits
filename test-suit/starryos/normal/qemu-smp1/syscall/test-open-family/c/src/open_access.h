#pragma once
/* 访问模式 (O_RDONLY/O_WRONLY/O_RDWR) × 实际 R/W/lseek 能力矩阵。
 * man 2 open: "flags must include one of the following access modes: O_RDONLY,
 * O_WRONLY, or O_RDWR." */
int open_access_run(void);
