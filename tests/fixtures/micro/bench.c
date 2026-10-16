/* Throughput benchmark: compute-bound phase, then an MMIO polling phase. */
#include <stdint.h>

extern uint32_t _estack;
volatile uint32_t sink;

static uint32_t crc32(const uint8_t *p, uint32_t n, uint32_t crc) {
    crc = ~crc;
    while (n--) {
        crc ^= *p++;
        for (int k = 0; k < 8; k++) crc = (crc >> 1) ^ (0xEDB88320u & -(crc & 1));
    }
    return ~crc;
}

static uint8_t buf[1024];

void Reset_Handler(void) {
    for (uint32_t i = 0; i < sizeof buf; i++) buf[i] = (uint8_t)(i * 7);
    uint32_t c = 0;
    for (int r = 0; r < 200; r++) c = crc32(buf, sizeof buf, c);
    sink = c;
    __asm volatile("bkpt #2");
    /* Polling phase: read a peripheral register in a tight loop (like
       waiting for a GPIO/timer flag). GPIOF->IDR on STM32F4. */
    volatile uint32_t *idr = (volatile uint32_t *)0x40021410;
    uint32_t acc = 0;
    for (uint32_t i = 0; i < 2000000; i++) acc += *idr;
    sink = acc;
    for (;;) __asm volatile("bkpt #0");
}

__attribute__((section(".vectors"), used)) void (*const vectors[2])(void) = {
    (void (*)(void))&_estack,
    Reset_Handler,
};
