/* Laid out like an ESP-IDF app: `.flash.rodata` is writable (WA) and in a
 * read-write PT_LOAD in DROM, and ROM functions and data are absolute
 * symbols, as in esp_rom's esp32c3.rom*.ld. */
ENTRY(call_start_cpu0)
PHDRS { irom PT_LOAD FLAGS(5); iram PT_LOAD FLAGS(5); drom PT_LOAD FLAGS(6); dram PT_LOAD FLAGS(6); }
SECTIONS {
  .flash.text 0x42000020 : { *(.text .text.*) } :irom
  .iram0.text 0x40380000 : { *(.exception_vectors.text) } :iram
  .flash.rodata 0x3c000020 : { *(.flash.rodata_writable) *(.rodata .rodata.* .srodata .srodata.*) } :drom
  .dram0.data 0x3fc88000 : { *(.data .data.*) __global_pointer$ = . + 0x400; *(.sdata .sdata.*) *(.sbss .sbss.*) *(.bss .bss.* COMMON) } :dram
}
PROVIDE ( ets_printf = 0x40000040 );
PROVIDE ( memset = 0x40000354 );
PROVIDE ( memcpy = 0x40000358 );
PROVIDE ( _global_impure_ptr = 0x3fcdffdc );
PROVIDE ( g_coex_param_ptr = 0x3fcdf834 );
PROVIDE ( ets_delay_us = 0x40000050 );
PROVIDE ( __udivdi3 = 0x400008ac );
PROVIDE ( __floatsidf = 0x400007fc );
PROVIDE ( __muldf3 = 0x40000848 );
PROVIDE ( __fixdfsi = 0x400007dc );
PROVIDE ( rom_spiflash_legacy_data = 0x3fcdfff0 );
