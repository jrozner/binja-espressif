/* Instructions only the ESP32-S3 implements, written by hand: the Processor
 * Instruction Extensions (PIE) in both the 3- and 4-byte formats. */
#include <stdint.h>

int signed_less(int a, int b) {
    int r;
    __asm__("salt %0, %1, %2" : "=a"(r) : "a"(a), "a"(b));
    return r;
}

void vector_add(int8_t *dst, const int8_t *a, const int8_t *b, int blocks) {
    for (int i = 0; i < blocks; i++) {
        __asm__ volatile("ee.vld.128.ip q0, %0, 16\n\t"
                         "ee.vld.128.ip q1, %1, 16\n\t"
                         "ee.vadds.s8 q2, q0, q1\n\t"
                         "ee.vst.128.ip q2, %2, 16"
                         : "+a"(a), "+a"(b), "+a"(dst) :: "memory");
    }
}

void dot_product(const int16_t *a, const int16_t *b) {
    __asm__ volatile("ee.zero.accx\n\t"
                     "ee.vld.128.ip q0, %0, 16\n\t"
                     "ee.vld.128.ip q1, %1, 16\n\t"
                     "ee.vmulas.s16.accx.ld.ip q2, %0, 16, q0, q1\n\t"
                     "ee.vmulas.s16.accx.ld.ip.qup q3, %1, -32, q0, q1, q2, q3\n\t"
                     "ee.srs.accx a2, a3, 0\n\t"
                     "rur.accx_0 a4\n\t"
                     "wur.sar_byte a5"
                     :: "a"(a), "a"(b) : "a2", "a3", "a4", "a5", "memory");
}

void misc_vector(void) {
    __asm__ volatile("ee.zero.q q4\n\t"
                     "ee.zero.qacc\n\t"
                     "ee.movi.32.a q1, a6, 2\n\t"
                     "ee.movi.32.q q2, a7, 3\n\t"
                     "ee.andq q0, q1, q2\n\t"
                     "ee.notq q3, q4\n\t"
                     "ee.vcmp.gt.s16 q5, q6, q7\n\t"
                     "ee.vzip.8 q0, q1\n\t"
                     "ee.src.q q0, q1, q2\n\t"
                     "ee.bitrev q1, a8\n\t"
                     "ee.vmax.s32 q2, q3, q4\n\t"
                     "ee.cmul.s16 q0, q1, q2, 3\n\t"
                     "ee.fft.r2bf.s16 q0, q1, q2, q3, 1\n\t"
                     "ee.vldbc.32.ip q0, a9, -4\n\t"
                     "ee.ldf.64.xp f1, f2, a10, a11\n\t"
                     "ee.stf.64.xp f3, f4, a12, a13\n\t"
                     "ld.qr q1, a2, 32\n\t"
                     "st.qr q2, a3, -16\n\t"
                     "mv.qr q3, q4\n\t"
                     "ee.set_bit_gpio_out 5\n\t"
                     "ee.get_gpio_in a2\n\t"
                     "rur.gpio_out a3\n\t"
                     "rsr.eraccess a4\n\t"
                     "mul.aa.ll a2, a3\n\t"
                     "add.s f0, f1, f2\n\t"
                     "nop"
                     ::: "a2", "a3", "a4", "memory");
}
