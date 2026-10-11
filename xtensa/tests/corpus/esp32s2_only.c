/* Instructions only the ESP32-S2 implements, written by hand. */
#include <stdint.h>

int signed_less(int a, int b) {
    int r;
    __asm__("salt %0, %1, %2" : "=a"(r) : "a"(a), "a"(b));
    return r;
}

unsigned unsigned_less(unsigned a, unsigned b) {
    unsigned r;
    __asm__("saltu %0, %1, %2" : "=a"(r) : "a"(a), "a"(b));
    return r;
}

uint32_t dedicated_gpio(uint32_t mask, uint32_t value) {
    uint32_t in;
    __asm__ volatile("set_bit_gpio_out 3\n\tclr_bit_gpio_out 0x81\n\t"
                     "wr_mask_gpio_out %1, %2\n\tget_gpio_in %0\n\t"
                     "rur.gpio_out a2\n\twur.gpio_out a3"
                     : "=a"(in) : "a"(mask), "a"(value) : "a2", "a3");
    return in;
}

uint32_t eraccess(uint32_t v) {
    __asm__ volatile("xsr.eraccess %0\n\twsr.memctl %0\n\trsr.windowbase %0" : "+a"(v));
    return v;
}
