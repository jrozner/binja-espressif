/* Laid out like an ESP-IDF app: `.flash.rodata` is writable (WA) and in a
 * read-write PT_LOAD, and ROM functions and data are absolute symbols, as in
 * esp_rom's esp32.rom*.ld. */
ENTRY(start)
PHDRS { irom PT_LOAD FLAGS(5); drom PT_LOAD FLAGS(6); dram PT_LOAD FLAGS(6); }
SECTIONS {
  .flash.text 0x400d0020 : { *(.literal .text .literal.* .text.*) } :irom
  .flash.rodata 0x3f400020 : { *(.flash.rodata_writable) *(.rodata .rodata.* .srodata*) } :drom
  .dram0.data 0x3ffb0000 : { *(.data .data.* .sdata*) *(.bss .bss.* COMMON) } :dram
}
PROVIDE ( ets_printf = 0x40007d54 );
PROVIDE ( memcpy = 0x4000c2c8 );
PROVIDE ( memset = 0x4000c44c );
PROVIDE ( _global_impure_ptr = 0x3ffae0b0 );
PROVIDE ( _ctype_ = 0x3ff96354 );
PROVIDE ( ets_delay_us = 0x40008534 );
PROVIDE ( __udivdi3 = 0x4000cff8 );
PROVIDE ( __floatsidf = 0x4000c944 );
PROVIDE ( __muldf3 = 0x4006358c );
PROVIDE ( __fixdfsi = 0x40002a78 );
PROVIDE ( g_rom_flashchip = 0x3ffae270 );
