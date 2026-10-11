/* Floating point: FPU instructions on the ESP32, soft-float calls on the ESP8266. */
float saxpy(float a, const float *x, float *y, int n) {
    float acc = 0;
    for (int i = 0; i < n; i++) {
        y[i] = a * x[i] + y[i];
        acc += y[i];
    }
    return acc;
}

float fdiv(float a, float b) { return a / b; }
float fneg_abs(float a) { return a < 0 ? -a : -(a * 2.0f); }
int ftoi(float a) { return (int)a; }
unsigned ftou(float a) { return (unsigned)a; }
float itof(int a) { return (float)a; }
float utof(unsigned a) { return (float)a; }
int fcmp(float a, float b) { return (a < b) + 2 * (a <= b) + 4 * (a == b) + 8 * (a != a); }
float fselect(float a, float b, int c) { return c ? a : b; }
double dmul(double a, double b) { return a * b + 1.5; }
float fmadd(float a, float b, float c) { return __builtin_fmaf(a, b, c); }
float fsqrt(float a) { return __builtin_sqrtf(a); }
