/* Calls, loops, atomics and hand-written system code via inline assembly. */
#include <stdint.h>

typedef int (*callback_t)(int);
extern int external_function(int);

int call_indirect(callback_t cb, int v) { return cb(v) + external_function(v); }

int many_args(int a, int b, int c, int d, int e, int f, int g, int h) {
    return a + b * c - d + e * f - g + h;
}

int call_many(int x) { return many_args(x, x + 1, x + 2, x + 3, x + 4, x + 5, x + 6, x + 7); }

void copy_words(uint32_t *dst, const uint32_t *src, int n) {
    for (int i = 0; i < n; i++) dst[i] = src[i] ^ 0x5a5a5a5a;
}

void memset_bytes(volatile uint8_t *dst, uint8_t v, unsigned n) {
    while (n--) *dst++ = v;
}

int atomic_add(int *p, int v) { return __atomic_fetch_add(p, v, __ATOMIC_SEQ_CST); }
int atomic_cas(int *p, int expected, int desired) {
    return __atomic_compare_exchange_n(p, &expected, desired, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
}

uint32_t read_ccount(void) {
    uint32_t v;
    __asm__ volatile("rsr.ccount %0" : "=a"(v));
    return v;
}

void set_ccompare(uint32_t v) { __asm__ volatile("wsr.ccompare0 %0\n\tisync" :: "a"(v)); }

uint32_t irq_disable(void) {
    uint32_t ps;
    __asm__ volatile("rsil %0, 3" : "=a"(ps));
    return ps;
}

void irq_restore(uint32_t ps) { __asm__ volatile("wsr.ps %0\n\trsync" :: "a"(ps) : "memory"); }

uint32_t swap_vecbase(uint32_t v) {
    __asm__ volatile("xsr.vecbase %0" : "+a"(v));
    return v;
}

void barriers(void) {
    __asm__ volatile("memw\n\textw\n\tisync\n\tdsync\n\tesync\n\tnop\n\twaiti 0\n\tsyscall\n\tbreak 1, 15");
}

void exception_vector(void) {
    __asm__ volatile(
        "rsr.exccause a2\n\trsr.excvaddr a3\n\trsr.epc1 a4\n\twsr.excsave1 a5\n\t"
        "rsr.interrupt a6\n\twsr.intclear a6\n\trsr.intenable a7\n\trfe\n\trfi 3\n\trfde");
}

void shifts(void) {
    __asm__ volatile("ssl a2\n\tsll a3, a4\n\tssr a5\n\tsrl a6, a7\n\tssai 7\n\tsrc a8, a9, a10\n\t"
                     "ssa8l a11\n\tsra a12, a13\n\tnsau a2, a3\n\tnsa a4, a5\n\tmul16u a2, a3, a4\n\t"
                     "mul16s a5, a6, a7\n\tmull a8, a9, a10");
}
