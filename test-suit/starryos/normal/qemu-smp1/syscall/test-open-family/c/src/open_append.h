#pragma once
/* O_APPEND 行为：每次 write 前 offset 自动定位到文件末尾，原子。
 * man 2 open §"O_APPEND". */
int open_append_run(void);
