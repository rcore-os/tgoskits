#include <sys/mman.h>  
#include <unistd.h>  
#include <stdio.h>  
#include <assert.h>  
#include <fcntl.h>  
#include <string.h>  
#include <errno.h>  
  
#define MAP_HUGE_2MB    (21 << MAP_HUGE_SHIFT)  
#define MAP_HUGE_1GB    (30 << MAP_HUGE_SHIFT)  
  
// 内存读写测试函数  
void test_memory_rw(void *ptr, size_t size, const char *page_type) {  
    printf("Testing %s memory read/write...\n", page_type);  
      
    // 写入测试数据  
    char *mem = (char*)ptr;  
    for (size_t i = 0; i < size && i < 1024; i++) {  
        mem[i] = (char)(i % 256);  
    }  
      
    // 读取并验证数据  
    for (size_t i = 0; i < size && i < 1024; i++) {  
        assert(mem[i] == (char)(i % 256));  
    }  
      
    printf("%s memory read/write test passed\n", page_type);  
}  
  
// 测试1: 单独分配、读写和释放  
void test_individual_alloc_rw_free() {  
    printf("========== START test_individual_alloc_rw_free ==========\n");  
      
    // 4KB 页面测试  
    void *ptr_4k = mmap(NULL, 4096, PROT_READ | PROT_WRITE,   
                        MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);  
    assert(ptr_4k != MAP_FAILED);  
    printf("4KB page allocated at %p\n", ptr_4k);  
    test_memory_rw(ptr_4k, 4096, "4KB");  
    assert(munmap(ptr_4k, 4096) == 0);  
    printf("4KB page freed\n");  
      
    // 2MB 页面测试  
    void *ptr_2m = mmap(NULL, 2 * 1024 * 1024, PROT_READ | PROT_WRITE,  
                        MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB | MAP_HUGE_2MB, -1, 0);  
    assert(ptr_2m != MAP_FAILED);  
    printf("2MB page allocated at %p\n", ptr_2m);  
    test_memory_rw(ptr_2m, 2 * 1024 * 1024, "2MB");  
    assert(munmap(ptr_2m, 2 * 1024 * 1024) == 0);  
    printf("2MB page freed\n");  
      
    // 1GB 页面测试  
    void *ptr_1g = mmap(NULL, 1024 * 1024 * 1024, PROT_READ | PROT_WRITE,  
                        MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB | MAP_HUGE_1GB, -1, 0);  
    assert(ptr_1g != MAP_FAILED);  
    printf("1GB page allocated at %p\n", ptr_1g);  
    test_memory_rw(ptr_1g, 1024 * 1024 * 1024, "1GB");  
    assert(munmap(ptr_1g, 1024 * 1024 * 1024) == 0);  
    printf("1GB page freed\n");  
      
    printf("========== END test_individual_alloc_rw_free ==========\n");  
}  
  
// 测试2: 统一分配、读写，统一释放  
void test_batch_alloc_rw_free() {  
    printf("========== START test_batch_alloc_rw_free ==========\n");  
      
    void *ptr_4k, *ptr_2m, *ptr_1g;  
      
    // 统一分配  
    ptr_4k = mmap(NULL, 4096, PROT_READ | PROT_WRITE,   
                  MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);  
    assert(ptr_4k != MAP_FAILED);  
    printf("Batch allocated 4KB page at %p\n", ptr_4k);  
      
    ptr_2m = mmap(NULL, 2 * 1024 * 1024, PROT_READ | PROT_WRITE,  
                  MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB | MAP_HUGE_2MB, -1, 0);  
    assert(ptr_2m != MAP_FAILED);  
    printf("Batch allocated 2MB page at %p\n", ptr_2m);  
      
    ptr_1g = mmap(NULL, 1024 * 1024 * 1024, PROT_READ | PROT_WRITE,  
                  MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB | MAP_HUGE_1GB, -1, 0);  
    assert(ptr_1g != MAP_FAILED);  
    printf("Batch allocated 1GB page at %p\n", ptr_1g);  
      
    // 统一读写测试  
    test_memory_rw(ptr_4k, 4096, "4KB batch");  
    test_memory_rw(ptr_2m, 2 * 1024 * 1024, "2MB batch");  
    test_memory_rw(ptr_1g, 1024 * 1024 * 1024, "1GB batch");  
      
    // 统一释放  
    assert(munmap(ptr_4k, 4096) == 0);  
    printf("Batch freed 4KB page\n");  
    assert(munmap(ptr_2m, 2 * 1024 * 1024) == 0);  
    printf("Batch freed 2MB page\n");  
    assert(munmap(ptr_1g, 1024 * 1024 * 1024) == 0);  
    printf("Batch freed 1GB page\n");  
      
    printf("========== END test_batch_alloc_rw_free ==========\n");  
}  
  
// 测试3: 交替分配、读写和释放  
void test_interleaved_alloc_rw_free() {  
    printf("========== START test_interleaved_alloc_rw_free ==========\n");  
      
    // 4KB -> 读写 -> 2MB -> 读写 -> 释放4KB -> 1GB -> 读写 -> 释放2MB -> 释放1GB  
    void *ptr_4k = mmap(NULL, 4096, PROT_READ | PROT_WRITE,   
                        MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);  
    assert(ptr_4k != MAP_FAILED);  
    printf("Interleaved: allocated 4KB page\n");  
    test_memory_rw(ptr_4k, 4096, "4KB interleaved");  
      
    void *ptr_2m = mmap(NULL, 2 * 1024 * 1024, PROT_READ | PROT_WRITE,  
                        MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB | MAP_HUGE_2MB, -1, 0);  
    assert(ptr_2m != MAP_FAILED);  
    printf("Interleaved: allocated 2MB page\n");  
    test_memory_rw(ptr_2m, 2 * 1024 * 1024, "2MB interleaved");  
      
    assert(munmap(ptr_4k, 4096) == 0);  
    printf("Interleaved: freed 4KB page\n");  
      
    void *ptr_1g = mmap(NULL, 1024 * 1024 * 1024, PROT_READ | PROT_WRITE,  
                        MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB | MAP_HUGE_1GB, -1, 0);  
    assert(ptr_1g != MAP_FAILED);  
    printf("Interleaved: allocated 1GB page\n");  
    test_memory_rw(ptr_1g, 1024 * 1024 * 1024, "1GB interleaved");  
      
    assert(munmap(ptr_2m, 2 * 1024 * 1024) == 0);  
    printf("Interleaved: freed 2MB page\n");  
      
    assert(munmap(ptr_1g, 1024 * 1024 * 1024) == 0);  
    printf("Interleaved: freed 1GB page\n");  
      
    printf("========== END test_interleaved_alloc_rw_free ==========\n");  
}  
  
// 测试4: 立即分配 vs 懒分配，包含读写测试  
void test_eager_vs_lazy_allocation() {  
    printf("========== START test_eager_vs_lazy_allocation ==========\n");  
      
    // 4KB 立即分配和懒分配  
    void *ptr_4k_eager = mmap(NULL, 4096, PROT_READ | PROT_WRITE,   
                              MAP_PRIVATE | MAP_ANONYMOUS | MAP_POPULATE, -1, 0);  
    assert(ptr_4k_eager != MAP_FAILED);  
    printf("4KB eager allocation completed\n");  
    test_memory_rw(ptr_4k_eager, 4096, "4KB eager");  
    assert(munmap(ptr_4k_eager, 4096) == 0);  
      
    void *ptr_4k_lazy = mmap(NULL, 4096, PROT_READ | PROT_WRITE,   
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);  
    assert(ptr_4k_lazy != MAP_FAILED);  
    printf("4KB lazy allocation completed\n");  
    test_memory_rw(ptr_4k_lazy, 4096, "4KB lazy");  
    assert(munmap(ptr_4k_lazy, 4096) == 0);  
      
    // 2MB 立即分配和懒分配  
    void *ptr_2m_eager = mmap(NULL, 2 * 1024 * 1024, PROT_READ | PROT_WRITE,  
                              MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB | MAP_HUGE_2MB | MAP_POPULATE, -1, 0);  
    assert(ptr_2m_eager != MAP_FAILED);  
    printf("2MB eager allocation completed\n");  
    test_memory_rw(ptr_2m_eager, 2 * 1024 * 1024, "2MB eager");  
    assert(munmap(ptr_2m_eager, 2 * 1024 * 1024) == 0);  
      
    void *ptr_2m_lazy = mmap(NULL, 2 * 1024 * 1024, PROT_READ | PROT_WRITE,  
                             MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB | MAP_HUGE_2MB, -1, 0);  
    assert(ptr_2m_lazy != MAP_FAILED);  
    printf("2MB lazy allocation completed\n");  
    test_memory_rw(ptr_2m_lazy, 2 * 1024 * 1024, "2MB lazy");  
    assert(munmap(ptr_2m_lazy, 2 * 1024 * 1024) == 0);  
      
    // 1GB 立即分配和懒分配  
    void *ptr_1g_eager = mmap(NULL, 1024 * 1024 * 1024, PROT_READ | PROT_WRITE,  
                              MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB | MAP_HUGE_1GB | MAP_POPULATE, -1, 0);  
    assert(ptr_1g_eager != MAP_FAILED);  
    printf("1GB eager allocation completed\n");  
    test_memory_rw(ptr_1g_eager, 1024 * 1024 * 1024, "1GB eager");  
    assert(munmap(ptr_1g_eager, 1024 * 1024 * 1024) == 0);  
      
    void *ptr_1g_lazy = mmap(NULL, 1024 * 1024 * 1024, PROT_READ | PROT_WRITE,  
                             MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB | MAP_HUGE_1GB, -1, 0);  
    assert(ptr_1g_lazy != MAP_FAILED);  
    printf("1GB lazy allocation completed\n");  
    test_memory_rw(ptr_1g_lazy, 1024 * 1024 * 1024, "1GB lazy");  
    assert(munmap(ptr_1g_lazy, 1024 * 1024 * 1024) == 0);  
      
    printf("All eager/lazy allocations freed\n");        
    printf("========== END test_eager_vs_lazy_allocation ==========\n");  
}  
  
// 测试5: 文件映射在不同页面大小  
void test_file_mapping_hugepages() {  
    printf("========== START test_file_mapping_hugepages ==========\n");  
      
    // 创建测试文件  
    int fd = open("/tmp/test_file", O_CREAT | O_RDWR, 0644);  
    assert(fd >= 0);  
      
    // 写入足够的测试数据  
    char test_data[4096];  
    memset(test_data, 'A', sizeof(test_data));  
    write(fd, test_data, sizeof(test_data));  
      
    // 4KB 页面文件映射  
    void *ptr_4k = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);  
    assert(ptr_4k != MAP_FAILED);  
    printf("4KB file mapping at %p\n", ptr_4k);  
    test_memory_rw(ptr_4k, 4096, "4KB file");  
    assert(munmap(ptr_4k, 4096) == 0);  
      
    // 扩展文件到2MB  
    lseek(fd, 2 * 1024 * 1024 - 1, SEEK_SET);  
    write(fd, "", 1);  
      
    // 2MB 页面文件映射  
    void *ptr_2m = mmap(NULL, 2 * 1024 * 1024, PROT_READ | PROT_WRITE,  
                        MAP_SHARED | MAP_HUGETLB | MAP_HUGE_2MB, fd, 0);  
  
    if (ptr_2m != MAP_FAILED) {  
        printf("2MB file mapping at %p\n", ptr_2m);  
        test_memory_rw(ptr_2m, 2 * 1024 * 1024, "2MB file");  
        assert(munmap(ptr_2m, 2 * 1024 * 1024) == 0);  
    } else {  
        printf("2MB file mapping failed, skipping\n");  
    }  

    close(fd);  
    unlink("/tmp/test_file");  
      
    printf("========== END test_file_mapping_hugepages ==========\n");  
}  
  
// 测试6: 线性映射  
void test_linear_mapping() {  
    printf("========== START test_linear_mapping ==========\n");  
      
    // 分配连续的不同页面大小区域  
    void *base_addr = (void*)0x08000000;  // 固定基地址  
      
    // 4KB 线性映射  
    void *ptr_4k = mmap(base_addr, 4096, PROT_READ | PROT_WRITE,   
                        MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);  
    if (ptr_4k != MAP_FAILED) {  
        printf("4KB linear mapping at %p\n", ptr_4k);  
        test_memory_rw(ptr_4k, 4096, "4KB linear");  
        assert(munmap(ptr_4k, 4096) == 0);  
        printf("4KB linear mapping freed\n");  
    } else {  
        printf("4KB linear mapping failed\n");  
    }  
      
    // 2MB 线性映射  
    void *ptr_2m = mmap((char*)base_addr + 0x1000000, 2 * 1024 * 1024,   
                        PROT_READ | PROT_WRITE,  
                        MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED | MAP_HUGETLB | MAP_HUGE_2MB,   
                        -1, 0);  
    if (ptr_2m != MAP_FAILED) {  
        printf("2MB linear mapping at %p\n", ptr_2m);  
        test_memory_rw(ptr_2m, 2 * 1024 * 1024, "2MB linear");  
        assert(munmap(ptr_2m, 2 * 1024 * 1024) == 0);  
        printf("2MB linear mapping freed\n");  
    } else {  
        printf("2MB linear mapping failed\n");  
    }  
      
    // 1GB 线性映射  
    void *ptr_1g = mmap((char*)base_addr + 0x20000000, 1024 * 1024 * 1024,   
                        PROT_READ | PROT_WRITE,  
                        MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED | MAP_HUGETLB | MAP_HUGE_1GB,   
                        -1, 0);  
    if (ptr_1g != MAP_FAILED && ptr_1g == (char*)base_addr + 0x20000000) {  
        printf("1GB linear mapping at %p\n", ptr_1g);  
        test_memory_rw(ptr_1g, 1024 * 1024 * 1024, "1GB linear");  
        assert(munmap(ptr_1g, 1024 * 1024 * 1024) == 0);  
        printf("1GB linear mapping freed\n");  
    } else {  
        printf("1GB linear mapping failed\n");  
    }  
      
    printf("========== END test_linear_mapping ==========\n");  
}  
  
int main() {  
    printf("Starting comprehensive hugepage tests\n");  
      
    test_individual_alloc_rw_free();  
    test_batch_alloc_rw_free();  
    test_interleaved_alloc_rw_free();  
    test_eager_vs_lazy_allocation();  
    test_file_mapping_hugepages();  
    test_linear_mapping();  
      
    printf("All hugepage tests completed successfully!\n");  
    return 0;  
}
