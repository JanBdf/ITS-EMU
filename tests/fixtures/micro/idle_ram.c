/* Idle detection: a loop whose registers repeat at its head while it counts
   in RAM. It must not be fast-forwarded (it reaches the BKPT after 200000
   iterations, at the same cycle with and without idle skipping). */
#include <stdint.h>

extern uint32_t _estack;
volatile uint32_t counter;

void Reset_Handler(void) {
    __asm volatile(
        "   ldr r1, =counter\n"
        "1: ldr r0, [r1]\n"
        "   adds r0, #1\n"
        "   str r0, [r1]\n"
        "   ldr r2, =200000\n"
        "   cmp r0, r2\n"
        "   beq 2f\n"
        "   movs r0, #0\n"
        "   movs r2, #0\n"
        "   b 1b\n"
        "2: bkpt #0\n" ::: "r0", "r1", "r2", "memory", "cc");
    for (;;) __asm volatile("bkpt #0");
}

__attribute__((section(".vectors"), used)) void (*const vectors[2])(void) = {
    (void (*)(void))&_estack,
    Reset_Handler,
};
