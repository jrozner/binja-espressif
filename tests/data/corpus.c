#include <stdint.h>
typedef struct { int a; short b; char c; float f; } rec_t;
volatile uint32_t sink;
__attribute__((noinline)) int add3(int a, int b, int c) { return a + b * c; }
__attribute__((noinline)) int many(int a, int b, int c, int d, int e, int f, int g, int h) { return a+b+c+d+e+f+g*h; }
__attribute__((noinline)) int64_t mul64(int64_t a, int64_t b) { return a * b; }
__attribute__((noinline)) int sum(const int *p, int n) { int s = 0; for (int i = 0; i < n; i++) s += p[i]; return s; }
__attribute__((noinline)) void copy(uint8_t *d, const uint8_t *s, int n) { while (n--) *d++ = *s++; }
__attribute__((noinline)) int sw(int x) { switch (x) { case 0: return 10; case 1: return 22; case 2: return 37; case 3: return 41; case 4: return 59; case 5: return 63; case 6: return 77; default: return -1; } }
__attribute__((noinline)) float fmix(float a, float b, int i) { return a * b + (float)i - (a < b ? a : b); }
__attribute__((noinline)) int bits(uint32_t x, uint32_t y) { if (x & (1u << 7)) return (x >> 3) & 0x1f; return (int)(x << (y & 31)) + __builtin_clz(y | 1); }
__attribute__((noinline)) int cas(volatile int *p, int o, int n) { return __sync_val_compare_and_swap(p, o, n); }
__attribute__((noinline)) int udiv(unsigned a, unsigned b) { return a / b + a % b; }
__attribute__((noinline)) int fieldsum(rec_t *r) { return r->a + r->b + r->c + (int)r->f; }
int (*fp)(int, int, int) = add3;
void start(void) {
  int arr[8] = {1,2,3,4,5,6,7,8};
  rec_t r = {1, 2, 3, 4.0f};
  sink = add3(1, 2, 3) + many(1,2,3,4,5,6,7,8) + (int)mul64(sink, 7) + sum(arr, 8) + sw(sink) + (int)fmix(1.0f, 2.0f, 3) + bits(sink, 3) + cas((volatile int*)&sink, 1, 2) + udiv(sink, 3) + fieldsum(&r) + fp(4,5,6);
  copy((uint8_t*)arr, (uint8_t*)&r, sizeof r);
  for (;;) {}
}
