/**
 * Emulator test program: counting delay loops as students and the display
 * library write them, built at -O0 like the course projects. Each runs a few
 * times with different counts while SysTick (HAL tick) interrupts it; the
 * cycles each took (DWT) go to the virtual COM port:
 *   D <loop> <count> <cycles>
 * The emulator must give the same lines with and without skipping loops.
 */
#include <stdio.h>
#include "stm32f4xx_hal.h"
#include "init.h"

void Driver_Delay_us(uint32_t xus);

static void volatile_up(uint32_t n) {
    for (volatile uint32_t i = 0; i < n; i++) {
    }
}

static void nop_up(int n) {
    for (int i = 0; i < n; i++) {
        __NOP();
    }
}

static void while_down(uint32_t n) {
    while (n--) {
    }
}

static void signed_down(int n) {
    for (volatile int i = n; i >= 0; i--) {
    }
}

static void measure(const char *name, void (*loop)(uint32_t), uint32_t n) {
    uint32_t t0 = DWT->CYCCNT;
    loop(n);
    uint32_t t1 = DWT->CYCCNT;
    printf("D %s %lu %lu\n", name, (unsigned long)n, (unsigned long)(t1 - t0));
}

static void call_nop_up(uint32_t n) {
    nop_up((int)n);
}

static void call_signed_down(uint32_t n) {
    signed_down((int)n);
}

int main(void) {
    initITSboard();
    CoreDebug->DEMCR |= CoreDebug_DEMCR_TRCENA_Msk;
    DWT->CTRL |= DWT_CTRL_CYCCNTENA_Msk;
    static const uint32_t counts[] = {0, 1, 7, 1000, 123457, 2000000};
    for (unsigned i = 0; i < sizeof counts / sizeof counts[0]; i++) {
        measure("driver_delay_us", Driver_Delay_us, counts[i]);
        measure("volatile_up", volatile_up, counts[i]);
        measure("nop_up", call_nop_up, counts[i]);
        measure("while_down", while_down, counts[i]);
        measure("signed_down", call_signed_down, counts[i]);
    }
    printf("DelayLoops done\n");
    while (1) {
    }
}
