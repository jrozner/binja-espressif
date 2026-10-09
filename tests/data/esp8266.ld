ENTRY(start)
SECTIONS {
  .text 0x40100000 : { *(.literal .text .literal.* .text.*) }
  .rodata 0x3ffe8000 : { *(.rodata .rodata.* .srodata*) }
  .data 0x3ffe9000 : { *(.data .data.* .sdata*) *(.bss .bss.* COMMON) }
}
