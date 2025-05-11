#include <sys/ipc.h>
#include <sys/shm.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <sys/wait.h>
#include <assert.h>

const int NUM = 10000;

int main() {
    key_t key = ftok("/tmp", 'A');
    struct shmid_ds buf;
    int shm_id;

    shm_id = shmget(key, NUM * sizeof(int), IPC_CREAT | 0666);
    if (shm_id == -1) {
        perror("shmget failed");
        return 1;
    }

    // 检查一遍 shm_ctl
    if (shmctl(shm_id, IPC_STAT, &buf) == -1) {
        perror("shmctl IPC_STAT failed");
        return;
    }
    assert(buf.shm_perm.__key == key);
    assert(buf.shm_cpid = getpid());
    assert(buf.shm_nattch == 0);
    assert(buf.shm_segsz == NUM * sizeof(int));
    

    pid_t pid = fork();
    if (pid == -1) {
        perror("fork failed");
        return 1;
    }

    if (pid == 0) { 
        int *shm_ptr = (int *)shmat(shm_id, NULL, 0);

        // 检查 shm_nattch
        struct shmid_ds buf1;
        shmctl(shm_id, IPC_STAT, &buf1);
        assert(buf1.shm_nattch == 1 || buf1.shm_nattch == 2);
        
        for (int i = 0; i < 10; i++) {
            shm_ptr[i] = i * i;
        }
        if (shmdt(shm_ptr)) {
            perror("shmdt failed in child");
            exit(1);
        }

        // 检查 shm_nattch
        shmctl(shm_id, IPC_STAT, &buf1);
        assert(buf1.shm_nattch == 1);
        
        exit(0);
    } else { 
        // 父进程先检查，再写入

        int *shm_ptr = (int *)shmat(shm_id, NULL, 0);

        // 检查 shm_nattch
        struct shmid_ds buf2;
        shmctl(shm_id, IPC_STAT, &buf2);
        assert(buf2.shm_nattch == 1 || buf2.shm_nattch == 2);
        
        // 等待子进程写入
        wait(NULL);
        
        // 检查 shm_nattch
        shmctl(shm_id, IPC_STAT, &buf2);
        assert(buf2.shm_nattch == 1);
        
        // 读取 shmem 并检查
        for (int i = 0; i < 10; i++) {
            assert(shm_ptr[i] == i * i);
        }
        
        if (shmdt(shm_ptr)) {
            perror("shmdt failed in parent");
            return 1;
        }
        
        // 检查 shm_nattch
        shmctl(shm_id, IPC_STAT, &buf2);
        assert(buf2.shm_nattch == 0);

        if (shmctl(shm_id, IPC_RMID, NULL) == -1) {
            perror("shmctl failed");
            return 1;
        }
        
        printf("shm check passed!\n");
        return 0;
    }

    return 0;
}

