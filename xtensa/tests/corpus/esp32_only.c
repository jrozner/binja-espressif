/* Instructions only the ESP32 implements, written by hand. */
#include <stdint.h>

uint32_t read_threadptr(void) {
    uint32_t v;
    __asm__ volatile("rur.threadptr %0" : "=a"(v));
    return v;
}

void mac16(void) {
    __asm__ volatile(
        "mul.aa.ll a2, a3\n\tmula.aa.hh a4, a5\n\tmuls.ad.lh a6, m2\n\tmul.da.hl m1, a7\n\t"
        "mula.dd.hh m0, m3\n\tumul.aa.hl a8, a9\n\tldinc m2, a10\n\tlddec m3, a11\n\t"
        "mula.da.ll.ldinc m1, a12, m0, a13\n\tmula.dd.hh.lddec m2, a14, m1, m3\n\t"
        "rsr.acclo a2\n\twsr.acchi a3\n\trsr.m0 a4\n\txsr.m3 a5");
}

void booleans(void) {
    __asm__ volatile("andb b0, b1, b2\n\torbc b3, b4, b5\n\txorb b6, b7, b8\n\tany4 b0, b4\n\t"
                     "all8 b1, b8\n\tmovt a2, a3, b4\n\tmovf a4, a5, b6\n\t"
                     "rsr.br a2\n\tbt b1, 1f\n\tbf b2, 1f\n1:");
}

void windowed(void) {
    __asm__ volatile("movsp a1, a2\n\trotw 1\n\trotw -1\n\tl32e a4, a1, -16\n\ts32e a5, a1, -12\n\t"
                     "rfwo\n\trfwu\n\trsr.windowbase a2\n\twsr.windowstart a3\n\tcallx4 a6\n\tcallx12 a7");
}

void misc(void) {
    __asm__ volatile("min a2, a3, a4\n\tmaxu a5, a6, a7\n\tsext a8, a9, 15\n\tclamps a10, a11, 7\n\t"
                     "quos a2, a3, a4\n\tremu a5, a6, a7\n\tmuluh a2, a3, a4\n\tmulsh a5, a6, a7\n\t"
                     "l32ai a2, a3, 4\n\ts32ri a4, a5, 8\n\twsr.scompare1 a2\n\ts32c1i a3, a4, 0\n\t"
                     "rsr.lbeg a2\n\trsr.lend a3\n\trsr.lcount a4");
}

void floats(void) {
    __asm__ volatile("lsi f0, a2, 4\n\tssi f1, a3, 8\n\tlsx f2, a4, a5\n\tssx f3, a6, a7\n\t"
                     "rfr a2, f4\n\twfr f5, a3\n\tmadd.s f0, f1, f2\n\tmsub.s f3, f4, f5\n\t"
                     "round.s a2, f0, 0\n\tfloor.s a3, f1, 2\n\tceil.s a4, f2, 3\n\tufloat.s f6, a5, 1\n\t"
                     "un.s b0, f1, f2\n\tult.s b1, f3, f4\n\tmovt.s f5, f6, b2\n\tmovltz.s f7, f8, a9\n\t"
                     "rur.fcr a2\n\twur.fsr a3\n\tdiv0.s f1, f2\n\tnexp01.s f3, f4\n\tmaddn.s f5, f6, f7");
}
