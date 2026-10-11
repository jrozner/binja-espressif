/* Relocations the Xtensa ELF relocation handler must refuse: each one, if
 * applied, would need an encoding the instruction does not have. Assembled
 * by scripts/build_test_elfs.sh into reloc_edge.o; tests/test_headless.py
 * checks that these instructions are left as assembled and the control
 * cases are relocated.
 *
 * Binary Ninja lays an object's sections out in section header order:
 * .early before .text.edge, .late after it, and .far (NOBITS, 1 MiB) after both.
 */
    .section .early, "ax"
    .align 4
early_literal:
    .word   0x87654321
early_target:
    ret.n

    .section .text.edge, "ax"
    .begin no-transform
    .align 4
    .global edge
    .type   edge, @function
edge:
    entry   a1, 32
    l32r    a2, late_literal    /* literal after its l32r */
    call8   odd_target          /* misaligned call target */
    call8   far_target          /* more than 512 KiB away */
    j       far_target          /* more than 128 KiB away */
    beqz    a2, far_target      /* more than 2 KiB away */
    bnez.n  a2, early_target    /* backwards: bnez.n only goes forward */
    l32r    a3, early_literal   /* control: relocated */
    call8   late_target         /* control: relocated */
    retw.n
    .end no-transform

    .section .late, "ax"
    .align 4
late_literal:
    .word   0x12345678
late_target:
    retw.n
    .byte   0
odd_target:
    retw.n

    .section .far, "aw", @nobits
    .skip   0x100000
    .align 4
far_target:
    .skip   4
