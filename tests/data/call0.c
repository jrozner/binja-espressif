// Built for the ESP32 with -mabi=call0: the non-windowed ABI on a windowed
// core, so no function begins with `entry` (see scripts/build_test_elfs.sh).
#include <stdint.h>
volatile int sink;
volatile int64_t sink64;
// In a section of its own, so that it is called with an l32r/callx0 pair
// (-mlongcalls, linked without relaxation).
__attribute__((noipa, section(".text.far"))) int add3(int a, int b, int c) { return a + b * c; }
__attribute__((noipa)) int64_t pair(int hi, unsigned lo) { return ((int64_t)hi << 32) | lo; }
__attribute__((noipa)) int twice(int x) { return add3(x, x, 1) + 1; }
int (*volatile op)(int, int, int) = add3;
void start(void) {
  sink = add3(1, 2, 3);
  int t = twice(sink);
  sink64 = pair(t, 5);
  // A call through an unknown pointer, with t kept across it in a12.
  sink = op(4, 5, 6) + t;
  for (;;) {}
}
