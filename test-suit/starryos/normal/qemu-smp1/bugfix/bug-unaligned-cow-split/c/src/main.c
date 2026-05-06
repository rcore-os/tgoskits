/*
 * bug-unaligned-cow-split: an ELF file-backed private segment with an
 * unaligned p_vaddr/p_offset pair should still fault in the correct file page
 * after mprotect() splits the VMA.
 *
 * The test embeds three file-backed pages in the RW PT_LOAD segment:
 *   page 0 -> 'A'
 *   page 1 -> 'B'
 *   page 2 -> 'C'
 *
 * We then mprotect() the middle page to force a left/middle/right split and
 * read from the untouched right page. A buggy COW split backend tends to leave
 * a zeroed gap or read the wrong file offset for that right-half fault.
 */
#define _GNU_SOURCE

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

enum {
    TEST_PAGE_SIZE = 4096,
    TEST_BLOB_PAGES = 3,
};

__asm__(
    ".pushsection .data\n"
    ".balign 4096\n"
    ".global bug_unaligned_cow_blob\n"
    "bug_unaligned_cow_blob:\n"
    ".fill 4096,1,0x41\n"
    ".fill 4096,1,0x42\n"
    ".fill 4096,1,0x43\n"
    ".global bug_unaligned_cow_blob_end\n"
    "bug_unaligned_cow_blob_end:\n"
    ".popsection\n");

extern unsigned char bug_unaligned_cow_blob[];
extern unsigned char bug_unaligned_cow_blob_end[];

static unsigned char read_byte(const volatile unsigned char *ptr)
{
    return *ptr;
}

int main(void)
{
    const long page_size = sysconf(_SC_PAGESIZE);
    const size_t blob_size = (size_t)(bug_unaligned_cow_blob_end - bug_unaligned_cow_blob);
    unsigned char *const middle_page = bug_unaligned_cow_blob + TEST_PAGE_SIZE;
    unsigned char *const right_page = bug_unaligned_cow_blob + TEST_PAGE_SIZE * 2;

    printf("=== bug-unaligned-cow-split ===\n");
    printf("Expected: file-backed private ELF segment still reads the correct\n");
    printf("          right-half page after mprotect() splits the VMA.\n\n");

    if (page_size != TEST_PAGE_SIZE) {
        printf("FAIL: unexpected page size %ld, expected %d\n",
               page_size, TEST_PAGE_SIZE);
        printf("TEST FAILED\n");
        return 1;
    }

    if (blob_size < TEST_PAGE_SIZE * TEST_BLOB_PAGES) {
        printf("FAIL: embedded blob too small: %zu bytes\n", blob_size);
        printf("TEST FAILED\n");
        return 1;
    }

    printf("blob=%p blob_size=%zu middle=%p right=%p\n",
           (void *)bug_unaligned_cow_blob, blob_size,
           (void *)middle_page, (void *)right_page);

    if (read_byte(bug_unaligned_cow_blob) != 'A') {
        printf("FAIL: first file-backed page mismatch before split\n");
        printf("TEST FAILED\n");
        return 1;
    }

    if (mprotect(middle_page, TEST_PAGE_SIZE, PROT_READ) != 0) {
        printf("FAIL: mprotect middle page to PROT_READ: %s\n", strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }

    if (read_byte(middle_page + 123) != 'B') {
        printf("FAIL: middle page content changed after split\n");
        printf("TEST FAILED\n");
        return 1;
    }

    if (read_byte(right_page) != 'C' ||
        read_byte(right_page + 137) != 'C' ||
        read_byte(right_page + TEST_PAGE_SIZE - 1) != 'C') {
        printf("FAIL: right page content mismatch after split\n");
        printf("      got bytes: [%u, %u, %u], expected all %u ('C')\n",
               read_byte(right_page),
               read_byte(right_page + 137),
               read_byte(right_page + TEST_PAGE_SIZE - 1),
               (unsigned int)'C');
        printf("TEST FAILED\n");
        return 1;
    }

    if (mprotect(middle_page, TEST_PAGE_SIZE, PROT_READ | PROT_WRITE) != 0) {
        printf("FAIL: restore middle page to PROT_READ|PROT_WRITE: %s\n",
               strerror(errno));
        printf("TEST FAILED\n");
        return 1;
    }

    middle_page[0] = 'b';
    if (read_byte(middle_page) != 'b' ||
        read_byte(right_page) != 'C' ||
        read_byte(bug_unaligned_cow_blob) != 'A') {
        printf("FAIL: page contents not isolated after restore/write\n");
        printf("TEST FAILED\n");
        return 1;
    }

    printf("PASS: right-half page retained correct file data after COW split\n");
    printf("TEST PASSED\n");
    return 0;
}
