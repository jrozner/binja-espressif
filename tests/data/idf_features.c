/* The ESP-IDF ELF layout quirks from KNOWN_ISSUES.md, for the ELF fixups:
 * a switch compiled to a jump table that the linker scripts put in a
 * writable `.flash.rodata`, calls to mask ROM functions that resolve to
 * `ABS` symbols, and reads of ROM data. ROM_DATA names a ROM data symbol
 * of the chip (see scripts/build_idf_elfs.sh). The ROM calls and the ROM
 * flash chip structure are for the ROM types (src/espressif/rom_types.rs). */

typedef unsigned int size_t;
typedef unsigned int uint32_t;
typedef unsigned long long uint64_t;

extern void *memcpy(void *dst, const void *src, size_t n);
extern void *memset(void *dst, int c, size_t n);
extern int ets_printf(const char *fmt, ...);
extern void ets_delay_us(uint32_t us);
extern void *_global_impure_ptr;
extern const unsigned int ROM_DATA;

/* The ROM's SPI flash chip description: on the ESP32 a variable of its own
 * (with -DROM_FLASHCHIP), on the later chips the start of the legacy data
 * the ROM points to (esp_rom_spiflash.h). */
struct flashchip {
    uint32_t device_id, chip_size, block_size, sector_size, page_size, status_mask;
};
#ifdef ROM_FLASHCHIP
extern struct flashchip g_rom_flashchip;
#define FLASH_SIZE() (g_rom_flashchip.chip_size)
#else
extern struct flashchip *rom_spiflash_legacy_data;
#define FLASH_SIZE() (rom_spiflash_legacy_data->chip_size)
#endif

uint32_t rom_calls(uint64_t total, uint32_t parts, int n);

/* ESP-IDF's `.flash.rodata` is writable because writable input sections are
 * placed in it; this one does the same here. */
__attribute__((section(".flash.rodata_writable"), used)) int flash_rodata_writable = 1;

/* Small data: gp-relative on RISC-V. */
int counter;
int last_op;

static char buf[32];

__attribute__((noinline)) int dispatch(int op, int arg)
{
    last_op = op;
    switch (op) {
    case 0: return arg + 1;
    case 1: memset(buf, arg, sizeof buf); return 7;
    case 2: memcpy(buf, &arg, sizeof arg); return buf[1];
    case 3: return ets_printf("op %d\n", arg);
    case 4: counter += arg; return counter;
    case 5: return arg * 3 - counter;
    case 6: return (int)(size_t)_global_impure_ptr;
    case 7: return (int)ROM_DATA ^ arg;
    case 8: counter = 0; return -1;
    case 9: return ets_printf("%s", buf) + arg;
    case 10: return arg >> 2;
    case 11: counter--; return arg & 0x55;
    default: return -2;
    }
}

int start(void)
{
    int sum = 0;
    for (int i = 0; i < 12; i++)
        sum += dispatch(i, sum);
    return sum + rom_calls(0x123456789ULL + sum, 3, sum);
}

/* Calls to ROM functions of one and three arguments, to the ROM's libgcc
 * helpers (64-bit division, soft-float double arithmetic), and a read of
 * the ROM's flash chip structure. */
__attribute__((noinline)) uint32_t rom_calls(uint64_t total, uint32_t parts, int n)
{
    ets_delay_us(parts);
    memset(buf, n, parts);
    double d = (double)n * 1.5;
    return (uint32_t)(total / parts) + (int)d + FLASH_SIZE();
}
