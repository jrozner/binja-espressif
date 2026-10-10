ENTRY(call_start_cpu0)
SECTIONS {
  .text 0x42000020 : { *(.text .text.*) }
  .iram0.text 0x40380000 : { *(.exception_vectors.text) }
  .rodata 0x3c000020 : { *(.rodata .rodata.* .srodata .srodata.*) }
  .data 0x3fc88000 : { *(.data .data.*) __global_pointer$ = . + 0x400; *(.sdata .sdata.*) *(.sbss .sbss.*) *(.bss .bss.* COMMON) }
}
