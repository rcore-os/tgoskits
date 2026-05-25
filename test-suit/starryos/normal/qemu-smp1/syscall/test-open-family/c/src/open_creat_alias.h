#pragma once
/* creat() == open(O_CREAT|O_WRONLY|O_TRUNC) 的等价性矩阵。
 * man 2 open §"creat()". */
int open_creat_alias_run(void);
