ENTRY(start)
SECTIONS {
  .text 0x42000020 : { *(.literal .text .literal.* .text.*) }
  .rodata 0x3c000020 : { *(.rodata .rodata.* .srodata*) }
  .data 0x3fc90000 : { *(.data .data.* .sdata*) *(.bss .bss.* COMMON) }
}
