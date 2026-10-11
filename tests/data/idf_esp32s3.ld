/* Laid out like an ESP-IDF app: `.flash.rodata` is writable (WA) and in a
 * read-write PT_LOAD, and ROM functions and data are absolute symbols, as in
 * esp_rom's esp32s3.rom*.ld. */
ENTRY(start)
PHDRS { irom PT_LOAD FLAGS(5); drom PT_LOAD FLAGS(6); dram PT_LOAD FLAGS(6); }
SECTIONS {
  .flash.text 0x42000020 : { *(.literal .text .literal.* .text.*) } :irom
  .flash.rodata 0x3c000020 : { *(.flash.rodata_writable) *(.rodata .rodata.* .srodata*) } :drom
  .dram0.data 0x3fc88000 : { *(.data .data.* .sdata*) *(.bss .bss.* COMMON) } :dram
}
PROVIDE ( ets_printf = 0x400005d0 );
PROVIDE ( memcpy = 0x400011f4 );
PROVIDE ( memset = 0x400011e8 );
PROVIDE ( _global_impure_ptr = 0x3fceffd0 );
PROVIDE ( g_coex_param_ptr = 0x3fcef820 );
PROVIDE ( ets_delay_us = 0x40000600 );
PROVIDE ( __udivdi3 = 0x40002544 );
PROVIDE ( __floatsidf = 0x40002334 );
PROVIDE ( __muldf3 = 0x40002418 );
PROVIDE ( __fixdfsi = 0x400022d4 );
PROVIDE ( rom_spiflash_legacy_data = 0x3fceffe4 );
