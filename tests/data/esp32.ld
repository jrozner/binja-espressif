ENTRY(start)
SECTIONS {
  .text 0x400d0020 : { *(.literal .text .literal.* .text.*) }
  .rodata 0x3f400020 : { *(.rodata .rodata.* .srodata*) }
  .data 0x3ffb0000 : { *(.data .data.* .sdata*) *(.bss .bss.* COMMON) }
}
