#include "test_framework.h"

int __pass = 0;
int __fail = 0;
int __skip = 0;
int __observe = 0;

extern int parts_dup_dup3_fcntl(void);
extern int parts_flock_basic(void);
extern int parts_fcntl_lock(void);
extern int parts_fcntl_rdlck(void);
extern int parts_flock_extra(void);

int main(void)
{
    int fail = 0;
    TEST_START("dup/dup3/fcntl/flock: full semantic validation v4");

    fail += parts_dup_dup3_fcntl();
    fail += parts_flock_basic();
    fail += parts_fcntl_lock();
    fail += parts_fcntl_rdlck();
    fail += parts_flock_extra();

    TEST_DONE();
}
