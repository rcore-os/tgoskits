#pragma once
/* O_DIRECTORY 在不同目标 + flag 组合下的行为。
 * man 2 open §"O_DIRECTORY": If pathname is not a directory, cause the open to fail. */
int open_directory_run(void);
