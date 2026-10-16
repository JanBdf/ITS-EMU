/* Emulator self-test: SysTick, SVC, FP context across interrupts, BusFault. */
#include <stdint.h>

#define REG(a) (*(volatile uint32_t *)(a))
#define SYST_CSR  REG(0xE000E010)
#define SYST_RVR  REG(0xE000E014)
#define SYST_CVR  REG(0xE000E018)
#define SCB_SHCSR REG(0xE000ED24)
#define SCB_CFSR  REG(0xE000ED28)
#define SCB_BFAR  REG(0xE000ED38)
#define SCB_CPACR REG(0xE000ED88)
#define STIR      REG(0xE000EF00)
#define NVIC_ISER0 REG(0xE000E100)

struct results {
    uint32_t magic;
    uint32_t ticks;
    uint32_t svc_arg;
    uint32_t irq5_count;
    uint32_t bfar;
    uint32_t cfsr;
    uint32_t fault_return_ok;
    float    fp_main;
    float    fp_isr;
    uint32_t data_init;
    uint32_t done;
};

volatile struct results results;
volatile uint32_t initialised = 0xC0FFEE;
volatile float isr_acc;

extern uint32_t _sidata, _sdata, _edata, _sbss, _ebss, _estack;
int main(void);

void Reset_Handler(void) {
    uint32_t *src = &_sidata, *dst = &_sdata;
    while (dst < &_edata) *dst++ = *src++;
    for (dst = &_sbss; dst < &_ebss;) *dst++ = 0;
    SCB_CPACR |= 0xF << 20; /* enable FPU */
    main();
    for (;;) __asm volatile("bkpt #0");
}

void SysTick_Handler(void) {
    results.ticks++;
    isr_acc = isr_acc * 1.5f + 0.25f; /* uses FP registers inside the ISR */
}

void IRQ5_Handler(void) { results.irq5_count++; }

void SVC_Handler_C(uint32_t *frame) {
    /* r0 of the caller is at frame[0] */
    results.svc_arg = frame[0] + 1;
}

__attribute__((naked)) void SVC_Handler(void) {
    __asm volatile("tst lr, #4\n ite eq\n mrseq r0, msp\n mrsne r0, psp\n b SVC_Handler_C\n");
}

void BusFault_Handler_C(uint32_t *frame) {
    results.cfsr = SCB_CFSR;
    results.bfar = SCB_BFAR;
    SCB_CFSR = SCB_CFSR; /* clear */
    frame[6] += 4;      /* skip the faulting 32-bit load */
}

__attribute__((naked)) void BusFault_Handler(void) {
    __asm volatile("tst lr, #4\n ite eq\n mrseq r0, msp\n mrsne r0, psp\n b BusFault_Handler_C\n");
}

void Default_Handler(void) { for (;;) __asm volatile("bkpt #1"); }

__attribute__((section(".vectors"), used)) void (*const vectors[16 + 8])(void) = {
    (void (*)(void))&_estack,
    Reset_Handler,
    Default_Handler,  /* NMI */
    Default_Handler,  /* HardFault */
    Default_Handler,  /* MemManage */
    BusFault_Handler,
    Default_Handler,  /* UsageFault */
    0, 0, 0, 0,
    SVC_Handler,
    Default_Handler,  /* DebugMon */
    0,
    Default_Handler,  /* PendSV */
    SysTick_Handler,
    Default_Handler, Default_Handler, Default_Handler, Default_Handler, Default_Handler,
    IRQ5_Handler,
    Default_Handler, Default_Handler,
};

__attribute__((noinline)) static uint32_t do_svc(uint32_t arg) {
    register uint32_t r0 __asm("r0") = arg;
    __asm volatile("svc #0" : "+r"(r0) :: "memory");
    return r0;
}

__attribute__((noinline)) static uint32_t read_unmapped(void) {
    uint32_t v;
    /* 32-bit LDR.W so the handler can skip exactly 4 bytes */
    __asm volatile("ldr.w %0, [%1]" : "=r"(v) : "r"(0x30000000u) : "memory");
    return v;
}

int main(void) {
    results.magic = 0x1234ABCD;
    results.data_init = initialised;

    /* SysTick every 1000 cycles, core clock */
    SYST_RVR = 999;
    SYST_CVR = 0;
    SYST_CSR = 7;

    /* FP work in thread mode while SysTick (which also uses FP) interrupts it.
       Lost FP context would change the bit-exact result. */
    float x = 1.0f;
    for (uint32_t i = 0; i < 50000; i++) {
        x = x * 0.999f + 0.5f;
    }
    while (results.ticks < 20) {
    }
    SYST_CSR = 0;
    results.fp_main = x;
    results.fp_isr = isr_acc;

    do_svc(41);

    /* software-triggered external interrupt 5 */
    NVIC_ISER0 = 1 << 5;
    STIR = 5;
    __asm volatile("dsb\n isb");

    SCB_SHCSR |= 1 << 17; /* enable BusFault */
    (void)read_unmapped();
    results.fault_return_ok = 1;

    results.done = 1;
    return 0;
}
