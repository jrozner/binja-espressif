/* Startup code for the RISC-V test ELFs, modelled on ESP-IDF: a vectored
 * interrupt table like components/riscv/vectors_intc.S and a
 * call_start_cpu0 that sets gp the way esp_system's init_cpu() does. */

    .section .exception_vectors.text, "ax"
    .option push
    .option norvc
    .option norelax             /* a fixed-size table: no linker-deleted padding */
    .balign 0x100
    .global _vector_table
_vector_table:
    j _panic_handler            /* 0: exception */
    .rept 23
    j _interrupt_handler        /* 1-23 */
    .endr
    j _panic_handler            /* 24, 25 */
    j _panic_handler
    .rept 6
    j _interrupt_handler        /* 26-31 */
    .endr
    .option pop

_panic_handler:
    j _panic_handler
_interrupt_handler:
    mret

    .section .text.call_start_cpu0, "ax"
    .global call_start_cpu0
call_start_cpu0:
    .option push
    .option norelax
    la gp, __global_pointer$
    .option pop
    la t0, _vector_table
    ori t0, t0, 1               /* vectored mode */
    csrw mtvec, t0
    tail start
