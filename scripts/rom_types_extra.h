/* Prototypes for ROM functions that no ESP-IDF or newlib header declares,
 * parsed by scripts/gen_rom_types.py alongside those headers.
 *
 * The libgcc support routines are compiler helpers: GCC calls them for 64-bit
 * integer arithmetic and for all floating point arithmetic on cores without
 * an FPU (or, for double, with a single-precision one), and the mask ROMs
 * carry a copy. Their prototypes are the ones the GCC internals manual gives
 * ("The GCC low-level runtime library"), with SItype = int, DItype = long
 * long, SFmode = float and DFmode = double, as on every Espressif core.
 *
 * The newlib internals are stdio routines that newlib's installed headers do
 * not declare, and ESP-IDF's ROM lock setup, declared only in a .c file.
 */

/* Integer arithmetic. */
int __ashlsi3(int a, int b);
long long __ashldi3(long long a, int b);
int __ashrsi3(int a, int b);
long long __ashrdi3(long long a, int b);
int __lshrsi3(int a, int b);
long long __lshrdi3(long long a, int b);
int __divsi3(int a, int b);
long long __divdi3(long long a, long long b);
int __modsi3(int a, int b);
long long __moddi3(long long a, long long b);
int __mulsi3(int a, int b);
long long __muldi3(long long a, long long b);
long long __negdi2(long long a);
unsigned int __udivsi3(unsigned int a, unsigned int b);
unsigned long long __udivdi3(unsigned long long a, unsigned long long b);
unsigned long long __udivmoddi4(unsigned long long a, unsigned long long b, unsigned long long *c);
long long __divmoddi4(long long a, long long b, long long *c);
unsigned int __umodsi3(unsigned int a, unsigned int b);
unsigned long long __umoddi3(unsigned long long a, unsigned long long b);
unsigned long long __umulsidi3(unsigned int a, unsigned int b);
int __cmpdi2(long long a, long long b);
int __ucmpdi2(unsigned long long a, unsigned long long b);

/* Overflow-trapping arithmetic. */
int __absvsi2(int a);
long long __absvdi2(long long a);
int __addvsi3(int a, int b);
long long __addvdi3(long long a, long long b);
int __subvsi3(int a, int b);
long long __subvdi3(long long a, long long b);
int __mulvsi3(int a, int b);
long long __mulvdi3(long long a, long long b);
int __negvsi2(int a);
long long __negvdi2(long long a);

/* Bit operations. */
int __clzsi2(unsigned int a);
int __clzdi2(unsigned long long a);
int __ctzsi2(unsigned int a);
int __ctzdi2(unsigned long long a);
int __ffssi2(unsigned int a);
int __ffsdi2(unsigned long long a);
int __paritysi2(unsigned int a);
int __paritydi2(unsigned long long a);
int __popcountsi2(unsigned int a);
int __popcountdi2(unsigned long long a);
int __bswapsi2(int a);
long long __bswapdi2(long long a);

/* Soft-float arithmetic and conversions. */
float __addsf3(float a, float b);
double __adddf3(double a, double b);
float __subsf3(float a, float b);
double __subdf3(double a, double b);
float __mulsf3(float a, float b);
double __muldf3(double a, double b);
float __divsf3(float a, float b);
double __divdf3(double a, double b);
float __negsf2(float a);
double __negdf2(double a);
float __powisf2(float a, int b);
double __powidf2(double a, int b);
double __extendsfdf2(float a);
float __truncdfsf2(double a);
int __fixsfsi(float a);
int __fixdfsi(double a);
long long __fixsfdi(float a);
long long __fixdfdi(double a);
unsigned int __fixunssfsi(float a);
unsigned int __fixunsdfsi(double a);
unsigned long long __fixunssfdi(float a);
unsigned long long __fixunsdfdi(double a);
float __floatsisf(int i);
double __floatsidf(int i);
float __floatdisf(long long i);
double __floatdidf(long long i);
float __floatunsisf(unsigned int i);
double __floatunsidf(unsigned int i);
float __floatundisf(unsigned long long i);
double __floatundidf(unsigned long long i);

/* Soft-float comparisons. */
int __cmpsf2(float a, float b);
int __cmpdf2(double a, double b);
int __unordsf2(float a, float b);
int __unorddf2(double a, double b);
int __eqsf2(float a, float b);
int __eqdf2(double a, double b);
int __nesf2(float a, float b);
int __nedf2(double a, double b);
int __gesf2(float a, float b);
int __gedf2(double a, double b);
int __ltsf2(float a, float b);
int __ltdf2(double a, double b);
int __lesf2(float a, float b);
int __ledf2(double a, double b);
int __gtsf2(float a, float b);
int __gtdf2(double a, double b);

/* newlib stdio internals (newlib's libc/stdio/local.h). */
struct _reent;
struct __sFILE;
void __sinit(struct _reent *ptr);
struct __sFILE *__sfp(struct _reent *ptr);
int __sflush_r(struct _reent *ptr, struct __sFILE *fp);
void __sfp_lock_acquire(void);
void __sfp_lock_release(void);
void __sinit_lock_acquire(void);
void __sinit_lock_release(void);

/* The ROM's newlib lock setup, as ESP-IDF's components/newlib/locks.c
 * declares it (_LOCK_T is a struct __lock pointer). */
struct __lock;
void esp_rom_newlib_init_common_mutexes(struct __lock *common_recursive_mutex, struct __lock *common_mutex);
