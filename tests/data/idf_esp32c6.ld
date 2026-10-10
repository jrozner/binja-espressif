/* Laid out like an ESP-IDF app: code and `.flash.rodata` share the flash
 * window, the rodata after the code in its own read-write PT_LOAD with
 * `.flash.rodata` writable (WA), and ROM functions and data are absolute
 * symbols, as in esp_rom's esp32c6.rom*.ld. */
ENTRY(call_start_cpu0)
PHDRS { irom PT_LOAD FLAGS(5); drom PT_LOAD FLAGS(6); iram PT_LOAD FLAGS(5); dram PT_LOAD FLAGS(6); }
SECTIONS {
  .flash.text 0x42000020 : { *(.text .text.*) } :irom
  .flash.rodata ALIGN(0x10) : { *(.flash.rodata_writable) *(.rodata .rodata.* .srodata .srodata.*) } :drom
  .iram0.text 0x40800000 : { *(.exception_vectors.text) } :iram
  .dram0.data 0x40808000 : { *(.data .data.*) __global_pointer$ = . + 0x400; *(.sdata .sdata.*) *(.sbss .sbss.*) *(.bss .bss.* COMMON) } :dram
}
PROVIDE ( ets_printf = 0x40000028 );
PROVIDE ( memset = 0x400004a8 );
PROVIDE ( memcpy = 0x400004ac );
PROVIDE ( _global_impure_ptr = 0x4087ffd0 );
PROVIDE ( crc8_be_table_ptr = 0x4004ffe4 );
