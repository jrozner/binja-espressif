ENTRY(call_start_cpu0)
SECTIONS {
  .text 0x42000020 : { *(.text .text.*) }
  .rodata : { *(.rodata .rodata.* .srodata .srodata.*) }
  .iram0.text 0x40800000 : { *(.exception_vectors.text) }
  .data 0x40808000 : { *(.data .data.*) __global_pointer$ = . + 0x400; *(.sdata .sdata.*) *(.sbss .sbss.*) *(.bss .bss.* COMMON) }
}
