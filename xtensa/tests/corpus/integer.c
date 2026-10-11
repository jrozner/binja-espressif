/* Integer workloads: the bulk of real firmware. */
#include <stdint.h>
#include <stddef.h>

uint32_t crc32(const uint8_t *data, size_t len) {
    uint32_t crc = 0xffffffffu;
    for (size_t i = 0; i < len; i++) {
        crc ^= data[i];
        for (int k = 0; k < 8; k++)
            crc = (crc >> 1) ^ (0xedb88320u & -(crc & 1));
    }
    return ~crc;
}

uint16_t fletcher16(const uint8_t *data, size_t len) {
    uint16_t a = 0, b = 0;
    while (len--) {
        a = (a + *data++) % 255;
        b = (b + a) % 255;
    }
    return (uint16_t)(b << 8 | a);
}

int32_t divide(int32_t a, int32_t b, int32_t *rem) {
    *rem = a % b;
    return a / b;
}

uint64_t mul64(uint64_t a, uint64_t b) { return a * b + (a >> 7) - (b << 3); }
int64_t div64(int64_t a, int64_t b) { return a / b; }

int popcount(uint32_t x) { return __builtin_popcount(x); }
int clz(uint32_t x) { return x ? __builtin_clz(x) : 32; }
int16_t sext16(int32_t x) { return (int16_t)x; }
int32_t clamp(int32_t x, int32_t lo, int32_t hi) { return x < lo ? lo : x > hi ? hi : x; }
uint32_t umax(uint32_t a, uint32_t b) { return a > b ? a : b; }
int32_t iabs(int32_t x) { return x < 0 ? -x : x; }
uint32_t rotl(uint32_t x, unsigned n) { return (x << (n & 31)) | (x >> ((32 - n) & 31)); }
uint32_t bitfield(uint32_t x) { return (x >> 5) & 0x3ff; }
int8_t load_s8(const int8_t *p) { return p[3]; }
int16_t load_s16(const int16_t *p) { return p[100]; }
void store_mixed(uint8_t *a, uint16_t *b, uint32_t *c, uint32_t v) { a[1] = v; b[2] = v; c[300] = v; }

int dispatch(int op, int a, int b) {
    switch (op) {
    case 0: return a + b;
    case 1: return a - b;
    case 2: return a * b;
    case 3: return a & b;
    case 4: return a | b;
    case 5: return a ^ b;
    case 6: return a << (b & 31);
    case 7: return a >> (b & 31);
    case 8: return (unsigned)a >> (b & 31);
    case 9: return a == b;
    default: return -1;
    }
}

struct node { struct node *next; int value; };

int list_sum(const struct node *n) {
    int sum = 0;
    for (; n; n = n->next) sum += n->value;
    return sum;
}

void bubble_sort(int *v, int n) {
    for (int i = 0; i < n; i++)
        for (int j = 0; j + 1 < n - i; j++)
            if (v[j] > v[j + 1]) { int t = v[j]; v[j] = v[j + 1]; v[j + 1] = t; }
}

uint32_t fib(uint32_t n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }

const char *find_char(const char *s, char c) {
    while (*s && *s != c) s++;
    return *s ? s : NULL;
}

uint32_t big_constants(uint32_t x) {
    return x * 0x9e3779b9u + 0x12345678u - (x ^ 0xdeadbeefu) + 2047 - 2049;
}
