/**
 * Emulator test program: hardware comparison.  Reads one command per line
 * from the virtual COM port (USART3), executes it and answers "ok ...".
 * The same command sequence is sent to the real board and to the emulator;
 * the display/LEDs are photographed resp. rendered and compared, timing
 * results are printed.
 *
 *   clear RRRR      GUI_clear(0xRRRR)
 *   bars            colour bars and RGB565 ramps
 *   orient          corner markers, 1-pixel frame, labels
 *   text            ITS_BRD_LIB fonts (lcdPrint*) and GUI_disStr
 *   geom            lines, rectangles, circles, points
 *   led XXXX        OUT15..0 = XXXX (GPIOD/E ODR bits 0..7)
 *   nled N          Nucleo LD1/LD2/LD3 = bits 0/1/2 of N
 *   bl N            backlight PWM compare value 0..1000 (TIM4->CCR4)
 *   in              prints GPIOF/GPIOG/GPIOC input levels
 *   pins            prints external levels of all input pins (ports A..I)
 *   time            prints durations (TIM2 ticks, 90 per us) of library calls
 *   cyc             prints core cycles (DWT->CYCCNT) of instruction kernels
 *   div             prints UDIV/SDIV cycles for a range of operands
 *   rd ADDR         prints the 32-bit word at ADDR
 *   wr ADDR VAL     writes the 32-bit word VAL to ADDR
 *   exti            EXTI/SYSCFG behaviour and interrupt latency (PG0, PG5)
 *   tim             timers: compare flags and interrupts, TIM1 repetition
 *                   counter, basic timers, shared interrupt lines
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "stm32f4xx_hal.h"
#include "init.h"
#include "lcd.h"
#include "LCD_GUI.h"
#include "fonts.h"
#include "timer.h"

extern int stdin_getchar(void);

static Coordinate xy(int x, int y) {
    Coordinate c = {(POINT)x, (POINT)y};
    return c;
}

static void fill(int x0, int y0, int x1, int y1, COLOR c) {
    GUI_drawRectangle(xy(x0, y0), xy(x1, y1), c, true, DOT_PIXEL_1X1);
}

static void cmd_bars(void) {
    static const COLOR bars[] = {WHITE, YELLOW, GBLUE, GREEN, MAGENTA, RED, BLUE, BLACK};
    for (int i = 0; i < 8; i++) {
        fill(i * 60, 0, i * 60 + 59, 159, bars[i]);
    }
    // Ramps, 32 steps of 15 pixels: grey, red, green, blue.
    for (int i = 0; i < 32; i++) {
        int x = i * 15;
        COLOR r = (COLOR)(i << 11), g = (COLOR)((i * 2) << 5), b = (COLOR)i;
        fill(x, 160, x + 14, 199, r | g | b);
        fill(x, 200, x + 14, 239, r);
        fill(x, 240, x + 14, 279, g);
        fill(x, 280, x + 14, 319, b);
    }
}

static void cmd_orient(void) {
    GUI_clear(WHITE);
    fill(0, 0, 59, 39, RED);
    fill(420, 0, 479, 39, GREEN);
    fill(0, 280, 59, 319, BLUE);
    fill(420, 280, 479, 319, BLACK);
    GUI_drawRectangle(xy(0, 0), xy(479, 319), BLACK, false, DOT_PIXEL_1X1);
    GUI_drawRectangle(xy(2, 2), xy(477, 317), BLACK, false, DOT_PIXEL_1X1);
    GUI_disStr(xy(70, 10), "TL (0,0)", &Font16, WHITE, BLACK);
    GUI_disStr(xy(290, 10), "TR (479,0)", &Font16, WHITE, BLACK);
    GUI_disStr(xy(70, 290), "BL (0,319)", &Font16, WHITE, BLACK);
    GUI_disStr(xy(270, 290), "BR (479,319)", &Font16, WHITE, BLACK);
    GUI_drawLine(xy(60, 40), xy(419, 279), RED, WHITE, true, DOT_PIXEL_2X2);
    GUI_disStr(xy(150, 150), "ITS-BRD", &Font24, WHITE, BLUE);
}

static void cmd_text(void) {
    GUI_clear(WHITE);
    // lcdGotoXY counts in lines of the current font.
    lcdSetFont(8);
    lcdGotoXY(0, 0);
    lcdPrintlnS("Font 8: AbgyQ09 {}|~ lcdPrint");
    lcdSetFont(12);
    lcdGotoXY(0, 1);
    lcdPrintlnS("Font 12: AbgyQ09 {}|~");
    lcdSetFont(16);
    lcdGotoXY(0, 2);
    lcdPrintlnS("Font 16: AbgyQ09 {}|~");
    lcdSetFont(20);
    lcdGotoXY(0, 3);
    lcdPrintS("Font 20: ");
    lcdPrintInt(-42);
    lcdSetFont(24);
    lcdGotoXY(0, 4);
    lcdPrintS("Font 24: AbgyQ09");
    GUI_disStr(xy(10, 160), "Font24 red/yellow", &Font24, YELLOW, RED);
    GUI_disStr(xy(10, 190), "Font20 white/black", &Font20, BLACK, WHITE);
    GUI_disStr(xy(10, 220), "Font16 blue on gray", &Font16, GRAY, BLUE);
    GUI_disNum(xy(10, 250), -1234567, &Font24, WHITE, BLACK);
    GUI_disChar(xy(400, 250), 'X', &Font24, CYAN, MAGENTA);
}

static void cmd_geom(void) {
    GUI_clear(WHITE);
    for (int i = 0; i < 8; i++) {
        GUI_drawLine(xy(10, 10 + i * 12), xy(150, 10 + i * 12 + i * 5), BLACK, WHITE, true,
                     (DOT_PIXEL)(i + 1));
    }
    GUI_drawLine(xy(10, 120), xy(150, 120), BLUE, WHITE, false, DOT_PIXEL_1X1);  // dotted
    GUI_drawRectangle(xy(170, 10), xy(300, 100), RED, false, DOT_PIXEL_3X3);
    GUI_drawRectangle(xy(320, 10), xy(470, 100), GREEN, true, DOT_PIXEL_1X1);
    GUI_drawCircle(xy(80, 220), 70, BLUE, false, DOT_PIXEL_2X2);
    GUI_drawCircle(xy(240, 220), 60, MAGENTA, true, DOT_PIXEL_1X1);
    GUI_drawCircle(xy(400, 220), 1, BLACK, false, DOT_PIXEL_1X1);
    for (int i = 0; i < 8; i++) {
        GUI_drawPoint(xy(330 + i * 18, 160), BLACK, (DOT_PIXEL)(i + 1), DOT_FILL_AROUND);
        GUI_drawPoint(xy(330 + i * 18, 290), RED, (DOT_PIXEL)(i + 1), DOT_FILL_RIGHTUP);
    }
}

static void cmd_led(unsigned v) {
    GPIOD->ODR = (GPIOD->ODR & ~0xFFu) | (v & 0xFF);
    GPIOE->ODR = (GPIOE->ODR & ~0xFFu) | ((v >> 8) & 0xFF);
}

static void cmd_nled(unsigned v) {
    // LD1 PB0, LD2 PB7, LD3 PB14 (outputs, configured here).
    __HAL_RCC_GPIOB_CLK_ENABLE();
    GPIOB->MODER = (GPIOB->MODER & ~((3u << 0) | (3u << 14) | (3u << 28))) |
                   (1u << 0) | (1u << 14) | (1u << 28);
    GPIOB->BSRR = ((v & 1) ? 1u << 0 : 1u << 16) | ((v & 2) ? 1u << 7 : 1u << 23) |
                  ((v & 4) ? 1u << 14 : 1u << 30);
}

static uint32_t elapsed(uint32_t t0) {
    return getTimeStamp() - t0;
}

static void cmd_time(void) {
    uint32_t t;
    t = getTimeStamp();
    GUI_clear(WHITE);
    printf("T clear %lu\n", (unsigned long)elapsed(t));

    t = getTimeStamp();
    GUI_disStr(xy(0, 0), "0123456789ABCDEFGHIJ", &Font24, WHITE, BLACK);
    printf("T str24 %lu\n", (unsigned long)elapsed(t));

    t = getTimeStamp();
    GUI_drawCircle(xy(240, 160), 100, BLUE, true, DOT_PIXEL_1X1);
    printf("T circle %lu\n", (unsigned long)elapsed(t));

    t = getTimeStamp();
    HAL_Delay(100);
    printf("T delay100 %lu\n", (unsigned long)elapsed(t));

    t = getTimeStamp();
    for (volatile uint32_t i = 0; i < 100000; i++) {
    }
    printf("T loop100k %lu\n", (unsigned long)elapsed(t));

    // Tight register-only loop: 1 subs + 1 bne per iteration.
    t = getTimeStamp();
    __asm volatile(
        "  mov r0, %0\n"
        "1: subs r0, r0, #1\n"
        "  bne 1b\n" ::"r"(100000) : "r0", "cc");
    printf("T asm100k %lu\n", (unsigned long)elapsed(t));

    t = getTimeStamp();
    printf("0123456789012345678901234567890123456789\n");
    printf("T printf41 %lu\n", (unsigned long)elapsed(t));
}

/* ---- Instruction timing: each kernel runs `n` iterations of a loop with
 * 10 copies of its body (r0 = n, r2 = argument). Cycles are measured with
 * DWT->CYCCNT; the empty kernel gives the loop overhead. */
#define KERNEL(name, setup, body)                                                     \
    __attribute__((naked, noinline)) static void k_##name(uint32_t n, uint32_t arg) { \
        __asm volatile("push {r4-r7, lr}\n"                                           \
                       "mov r2, r1\n" setup "\n"                                      \
                       ".balign 16\n"                                                 \
                       "1:\n"                                                         \
                       ".rept 10\n" body "\n.endr\n"                                  \
                       "subs r0, r0, #1\n"                                            \
                       "bne 1b\n"                                                     \
                       "pop {r4-r7, pc}\n");                                          \
    }

extern uint8_t SPI4W_Write_Byte(uint8_t value);

// Same as the static helper in LCD_SPI.c (GPIOA AFR read-modify-write).
__attribute__((used, noinline)) void save_restore_AltFn_SPI1_MOSI_Pin_copy(void) {
    const int pos = 7 * 4;
    static uint32_t saved_AF = GPIO_AF5_SPI1 << pos;
    uint32_t tmp = GPIOA->AFR[0] & (0xfU << pos);
    GPIOA->AFR[0] = (GPIOA->AFR[0] & ~(0xfU << pos)) | saved_AF;
    saved_AF = tmp;
}

__attribute__((naked, noinline, used)) void k_leaf(void) {
    __asm volatile("bx lr\n");
}

static uint32_t kbuf[16];

KERNEL(empty, "", "")
KERNEL(nop, "", "nop")
KERNEL(add, "movs r1, #0", "adds r1, r1, #1")
KERNEL(add_w, "movs r1, #0", "add.w r1, r1, #1")
KERNEL(mul, "movs r1, #3", "muls r1, r1, r1")
KERNEL(mla, "movs r1, #3\nmovs r3, #5", "mla r1, r1, r3, r1")
KERNEL(umull, "movs r1, #3\nmovs r3, #5", "umull r4, r5, r1, r3")
KERNEL(udiv_small, "movs r1, #100\nmovs r3, #7", "udiv r4, r1, r3")
KERNEL(udiv_big, "mvn r1, #0\nmovs r3, #7", "udiv r4, r1, r3")
KERNEL(ldr_sram, "", "ldr r1, [r2]")
KERNEL(ldr_ldr_sram, "", "ldr r1, [r2]\nldr r3, [r2, #4]")
KERNEL(ldr_use, "", "ldr r1, [r2]\nadds r1, r1, #1")
KERNEL(ldr_addr_dep, "str r2, [r2]", "ldr r3, [r2]\nldr r1, [r3]")
KERNEL(str_sram, "movs r1, #0", "str r1, [r2]")
KERNEL(str_str_sram, "movs r1, #0", "str r1, [r2]\nstr r1, [r2, #4]")
KERNEL(ldrb_sram, "", "ldrb r1, [r2]")
KERNEL(ldr_flash, "", "ldr r1, [r2]")
KERNEL(ldr_lit, "", "ldr r1, =0x12345678")
KERNEL(b_taken, "", "b 2f\n2:")
KERNEL(b_w_taken, "", "b.w 2f\n2:")
KERNEL(bne_taken, "movs r1, #1\ncmp r1, #0", "bne 2f\n2:")
KERNEL(bne_not, "movs r1, #0\ncmp r1, #0", "bne 2f\n2:")
KERNEL(cbz_taken, "movs r1, #0", "cbz r1, 2f\n2:")
KERNEL(bl_bx, "", "bl k_leaf")
KERNEL(blx_bx, "ldr r3, =k_leaf", "blx r3")
KERNEL(it_add, "movs r1, #0\ncmp r1, #0", "it eq\naddeq r1, r1, #1")
KERNEL(push_pop4, "", "push {r4-r7}\npop {r4-r7}")
KERNEL(ldm4, "", "ldm r2, {r4-r7}")
KERNEL(stm4, "", "stm r2, {r4-r7}")
KERNEL(ldr_ahb1, "", "ldr r1, [r2]")
KERNEL(str_ahb1, "movs r1, #0", "str r1, [r2]")
KERNEL(ldr_apb1, "", "ldr r1, [r2]")
KERNEL(ldr_apb2, "", "ldr r1, [r2]")
KERNEL(str_apb1, "movs r1, #0", "str r1, [r2]")
KERNEL(ldr_eth, "", "ldr r1, [r2]")
KERNEL(str_eth, "ldr r1, [r2]", "str r1, [r2]")
KERNEL(ldr_ahb1_nop, "", "ldr r1, [r2]\nnop")
KERNEL(ldr_apb1_nop, "", "ldr r1, [r2]\nnop")
// r2 = peripheral register, r3 = kbuf (SRAM)
KERNEL(stio_ldsram, "ldr r3, =kbuf\nmovs r1, #0", "str r1, [r2]\nldr r4, [r3]")
KERNEL(stio_stsram, "ldr r3, =kbuf\nmovs r1, #0", "str r1, [r2]\nstr r1, [r3]")
KERNEL(stio_alu, "movs r1, #0", "str r1, [r2]\nadds r4, r4, #1")
KERNEL(stsram_ldio, "ldr r3, =kbuf\nmovs r1, #0", "str r1, [r3]\nldr r4, [r2]")
KERNEL(ldio_stsram, "ldr r3, =kbuf", "ldr r4, [r2]\nstr r4, [r3]")
KERNEL(ldio_alu, "", "ldr r4, [r2]\nadds r4, r4, #1")
KERNEL(stsram_ldsram, "ldr r3, =kbuf\nmovs r1, #0", "str r1, [r3]\nldr r4, [r3, #4]")
// Branch targets of different width/alignment (body sizes keep alignment fixed)
KERNEL(b_to_n_al, "", "b 2f\nnop\n2: nop\nnop")             // 8 bytes: target 16-bit, word aligned
KERNEL(b_to_n_un, "", "nop\nb 2f\n2: nop\nnop")             // target 16-bit, unaligned
KERNEL(b_to_w_al, "", "b 2f\nnop\n2: add.w r1, r1, #1")     // target 32-bit, aligned
KERNEL(b_to_w_un, "", "b 2f\n2: add.w r1, r1, #1\nnop")     // target 32-bit, unaligned
KERNEL(w_run_un, "", "nop\nadd.w r1, r1, #1\nadd.w r1, r1, #1\nadd.w r1, r1, #1\nnop")
KERNEL(str_ldr_same, "ldr r3, =kbuf\nmovs r1, #0", "str r1, [r3]\nldr r4, [r3]")
KERNEL(str_b_ldr, "ldr r3, =kbuf\nmovs r1, #0", "str r1, [r3]\nb 2f\n2: ldr r4, [r3]\nnop")
KERNEL(ldr_sp_add_str, "", "ldr r1, [sp]\nadds r1, r1, #0\nstr r1, [sp]\nnop")
KERNEL(gpio_rmw, "", "ldr r4, [r2]\nbic.w r4, r4, #0xf0000000\nstr r4, [r2]")
KERNEL(gpio_rmw_sram, "ldr r3, =kbuf", "ldr r4, [r2]\nbic.w r4, r4, #0xf0000000\nstr r4, [r2]\nldr r5, [r3]\nstr r5, [r3, #4]")
KERNEL(gpio_ld_and_st, "ldr r3, =kbuf", "ldr r4, [r2]\nand.w r4, r4, #0xf0000000\nstr r4, [r3]")
KERNEL(vadd, "vmov.f32 s0, #1.0\nvmov.f32 s1, #1.0", "vadd.f32 s0, s0, s1")
KERNEL(vadd_indep, "vmov.f32 s0, #1.0\nvmov.f32 s1, #1.0", "vadd.f32 s2, s0, s1")
KERNEL(vmul, "vmov.f32 s0, #1.0\nvmov.f32 s1, #1.0", "vmul.f32 s0, s0, s1")
KERNEL(vdiv, "vmov.f32 s0, #1.0\nvmov.f32 s1, #1.0", "vdiv.f32 s0, s0, s1")
KERNEL(vsqrt, "vmov.f32 s0, #1.0", "vsqrt.f32 s0, s0")
KERNEL(vldr, "", "vldr s0, [r2]")
KERNEL(vmov_core, "vmov.f32 s0, #1.0", "vmov r1, s0")

// Division with dividend r2 (argument) and divisor r1 (set before the call).
__attribute__((naked, noinline)) static void k_udiv_var(uint32_t n, uint32_t dividend, uint32_t divisor) {
    __asm volatile("push {r4-r7, lr}\n"
                   ".balign 16\n"
                   "1:\n"
                   ".rept 10\nudiv r4, r1, r2\n.endr\n"
                   "subs r0, r0, #1\n"
                   "bne 1b\n"
                   "pop {r4-r7, pc}\n");
}

__attribute__((naked, noinline)) static void k_sdiv_var(uint32_t n, uint32_t dividend, uint32_t divisor) {
    __asm volatile("push {r4-r7, lr}\n"
                   ".balign 16\n"
                   "1:\n"
                   ".rept 10\nsdiv r4, r1, r2\n.endr\n"
                   "subs r0, r0, #1\n"
                   "bne 1b\n"
                   "pop {r4-r7, pc}\n");
}

static void cmd_div(void) {
    static const uint32_t divisors[] = {1, 7, 255, 65535, 0x7FFFFFFF};
    CoreDebug->DEMCR |= CoreDebug_DEMCR_TRCENA_Msk;
    DWT->CTRL |= DWT_CTRL_CYCCNTENA_Msk;
    for (unsigned d = 0; d < sizeof(divisors) / sizeof(divisors[0]); d++) {
        for (int k = 0; k <= 32; k += 1) {
            uint32_t dividend = k == 32 ? 0xFFFFFFFFu : (1u << k) - 1 + (k > 0);
            __disable_irq();
            uint32_t t0 = DWT->CYCCNT;
            k_udiv_var(100, dividend, divisors[d]);
            uint32_t t1 = DWT->CYCCNT;
            k_sdiv_var(100, dividend, (uint32_t)-(int32_t)divisors[d]);
            uint32_t t2 = DWT->CYCCNT;
            __enable_irq();
            printf("D %lu %lu %lu %lu\n", (unsigned long)dividend, (unsigned long)divisors[d],
                   (unsigned long)(t1 - t0), (unsigned long)(t2 - t1));
        }
    }
}

struct Kernel {
    const char *name;
    void (*fn)(uint32_t, uint32_t);
    uint32_t arg;
};

#define K(name, arg) {#name, k_##name, (uint32_t)(arg)}

static void cmd_cyc(void) {
    static const struct Kernel kernels[] = {
        K(empty, kbuf), K(nop, kbuf), K(add, kbuf), K(add_w, kbuf), K(mul, kbuf), K(mla, kbuf),
        K(umull, kbuf), K(udiv_small, kbuf), K(udiv_big, kbuf), K(ldr_sram, kbuf),
        K(ldr_ldr_sram, kbuf), K(ldr_use, kbuf), K(ldr_addr_dep, kbuf), K(str_sram, kbuf),
        K(str_str_sram, kbuf), K(ldrb_sram, kbuf), K(ldr_flash, 0x08000100), K(ldr_lit, kbuf),
        K(b_taken, kbuf), K(b_w_taken, kbuf), K(bne_taken, kbuf), K(bne_not, kbuf),
        K(cbz_taken, kbuf), K(bl_bx, kbuf), K(blx_bx, kbuf), K(it_add, kbuf),
        K(push_pop4, kbuf), K(ldm4, kbuf), K(stm4, kbuf),
        K(ldr_ahb1, &GPIOF->IDR), K(str_ahb1, &GPIOD->BSRR), K(ldr_apb1, &TIM2->CNT),
        K(ldr_apb2, &SPI1->SR), K(str_apb1, &TIM5->CCR4),
        K(ldr_eth, &ETH->DMASR), K(str_eth, &ETH->MACCR), K(ldr_ahb1_nop, &GPIOF->IDR),
        K(ldr_apb1_nop, &TIM2->CNT),
        K(stio_ldsram, &GPIOD->BSRR), K(stio_stsram, &GPIOD->BSRR), K(stio_alu, &GPIOD->BSRR),
        K(stsram_ldio, &GPIOF->IDR), K(ldio_stsram, &GPIOF->IDR), K(ldio_alu, &GPIOF->IDR),
        K(stsram_ldsram, kbuf),
        K(b_to_n_al, kbuf), K(b_to_n_un, kbuf), K(b_to_w_al, kbuf), K(b_to_w_un, kbuf),
        K(w_run_un, kbuf), K(str_ldr_same, kbuf), K(str_b_ldr, kbuf), K(ldr_sp_add_str, kbuf),
        K(gpio_rmw, &GPIOG->AFR[0]), K(gpio_rmw_sram, &GPIOG->AFR[0]),
        K(gpio_ld_and_st, &GPIOG->AFR[0]),
        K(vadd, kbuf), K(vadd_indep, kbuf), K(vmul, kbuf), K(vdiv, kbuf), K(vsqrt, kbuf),
        K(vldr, kbuf), K(vmov_core, kbuf),
    };
    CoreDebug->DEMCR |= CoreDebug_DEMCR_TRCENA_Msk;
    DWT->CTRL |= DWT_CTRL_CYCCNTENA_Msk;
    const uint32_t n = 1000;
    for (unsigned i = 0; i < sizeof(kernels) / sizeof(kernels[0]); i++) {
        __disable_irq();
        uint32_t t0 = DWT->CYCCNT;
        kernels[i].fn(n, kernels[i].arg);
        uint32_t t1 = DWT->CYCCNT;
        __enable_irq();
        printf("C %s %lu\n", kernels[i].name, (unsigned long)(t1 - t0));
    }
    // Library functions, 1000 calls each.
    uint32_t t0 = DWT->CYCCNT;
    for (int i = 0; i < 1000; i++) {
        SPI4W_Write_Byte(0);
    }
    uint32_t t1 = DWT->CYCCNT;
    printf("F SPI4W_Write_Byte %lu\n", (unsigned long)(t1 - t0));
    t0 = DWT->CYCCNT;
    for (int i = 0; i < 1000; i++) {
        GUI_drawPoint(xy(i % 100, 300), BLACK, DOT_PIXEL_1X1, DOT_FILL_AROUND);
    }
    t1 = DWT->CYCCNT;
    printf("F GUI_drawPoint %lu\n", (unsigned long)(t1 - t0));
    t0 = DWT->CYCCNT;
    for (int i = 0; i < 1000; i++) {
        save_restore_AltFn_SPI1_MOSI_Pin_copy();
    }
    t1 = DWT->CYCCNT;
    printf("F afr_save_restore %lu\n", (unsigned long)(t1 - t0));
}

/* External levels of input pins: pins configured as plain inputs are read
 * with the internal pull-down and pull-up; a pin that stays high against the
 * pull-down (low against the pull-up) is driven externally. */
static void cmd_pins(void) {
    GPIO_TypeDef *const ports[] = {GPIOA, GPIOB, GPIOC, GPIOD, GPIOE, GPIOF, GPIOG, GPIOH, GPIOI};
    RCC->AHB1ENR |= 0x1FF;
    for (unsigned p = 0; p < sizeof(ports) / sizeof(ports[0]); p++) {
        GPIO_TypeDef *g = ports[p];
        uint32_t moder = g->MODER, pupdr = g->PUPDR, inputs = 0, pd = 0, pu = 0;
        for (int pin = 0; pin < 16; pin++) {
            if (((moder >> (2 * pin)) & 3) == 0) {
                inputs |= 1u << pin;
                pd |= 2u << (2 * pin);
                pu |= 1u << (2 * pin);
            }
        }
        uint32_t keep = pupdr;
        for (int pin = 0; pin < 16; pin++) {
            if (inputs & (1u << pin)) {
                keep &= ~(3u << (2 * pin));
            }
        }
        uint32_t idr = g->IDR & 0xFFFF;
        g->PUPDR = keep | pd;
        HAL_Delay(2);
        uint32_t idr_pd = g->IDR & 0xFFFF;
        g->PUPDR = keep | pu;
        HAL_Delay(2);
        uint32_t idr_pu = g->IDR & 0xFFFF;
        g->PUPDR = pupdr;
        printf("P %c moder %08lx pupdr %08lx odr %04lx inputs %04lx idr %04lx pd %04lx pu %04lx\n",
               'A' + p, (unsigned long)moder, (unsigned long)pupdr, (unsigned long)(g->ODR & 0xFFFF),
               (unsigned long)inputs, (unsigned long)idr, (unsigned long)idr_pd, (unsigned long)idr_pu);
    }
}

/* ---- EXTI: PG0 and PG5 (AUX0/AUX5, open on the board) are driven as
 * outputs; the input stage still feeds the EXTI edge detectors. */
static volatile uint32_t exti_count, exti_cyc, exti_pr;

static void exti_isr(uint32_t line) {
    exti_cyc = DWT->CYCCNT;
    exti_pr = EXTI->PR;
    EXTI->PR = line;
    exti_count++;
}

void EXTI0_IRQHandler(void) { exti_isr(1u << 0); }
void EXTI9_5_IRQHandler(void) { exti_isr(1u << 5); }

static void pg(int pin, int level) {
    GPIOG->BSRR = level ? 1u << pin : 1u << (pin + 16);
    __DSB();
}

#define SHOW(name, expr) printf("E %s %08lx\n", name, (unsigned long)(expr))

static void cmd_exti(void) {
    CoreDebug->DEMCR |= CoreDebug_DEMCR_TRCENA_Msk;
    DWT->CTRL |= DWT_CTRL_CYCCNTENA_Msk;
    RCC->AHB1ENR |= RCC_AHB1ENR_GPIOGEN;
    SHOW("reset_imr", EXTI->IMR);
    SHOW("reset_rtsr", EXTI->RTSR);
    SHOW("reset_pr", EXTI->PR);
    SHOW("reset_exticr1", SYSCFG->EXTICR[0]);
    SHOW("apb2enr", RCC->APB2ENR);

    RCC->APB2ENR &= ~RCC_APB2ENR_SYSCFGEN;
    __DSB();
    SYSCFG->EXTICR[0] = 0x6;
    SHOW("exticr_noclk", SYSCFG->EXTICR[0]);
    RCC->APB2ENR |= RCC_APB2ENR_SYSCFGEN;
    __DSB();
    SHOW("exticr_clk_after_noclk_write", SYSCFG->EXTICR[0]);
    SYSCFG->EXTICR[0] = 0xFFFFFFFF;
    SHOW("exticr1_ones", SYSCFG->EXTICR[0]);
    SYSCFG->EXTICR[0] = 0x6;      // line 0 = PG0
    SYSCFG->EXTICR[1] = 0x6 << 4; // line 5 = PG5
    SHOW("exticr1", SYSCFG->EXTICR[0]);

    EXTI->IMR = 0xFFFFFFFF;
    SHOW("imr_ones", EXTI->IMR);
    EXTI->RTSR = 0xFFFFFFFF;
    SHOW("rtsr_ones", EXTI->RTSR);
    EXTI->IMR = 0;
    EXTI->RTSR = 0;
    EXTI->FTSR = 0;
    EXTI->PR = 0xFFFFFFFF;
    SHOW("pr_after_clear", EXTI->PR);

    GPIOG->MODER = (GPIOG->MODER & ~((3u << 0) | (3u << 10))) | (1u << 0) | (1u << 10);
    pg(0, 0);
    pg(5, 0);

    // edges with the line masked
    EXTI->RTSR = 1u << 0;
    pg(0, 1);
    SHOW("pr_rise_masked", EXTI->PR);
    EXTI->PR = 0xFFFFFFFF;
    pg(0, 0);
    SHOW("pr_fall_rtsr_only", EXTI->PR);
    EXTI->IMR = 1u << 0;
    pg(0, 1);
    SHOW("pr_rise_unmasked", EXTI->PR);
    EXTI->PR = 1u << 1;
    SHOW("pr_clear_other", EXTI->PR);
    EXTI->PR = 1u << 0;
    SHOW("pr_clear", EXTI->PR);
    EXTI->FTSR = 1u << 0;
    pg(0, 0);
    SHOW("pr_fall_ftsr", EXTI->PR);
    EXTI->PR = 1u << 0;
    EXTI->IMR = 0;
    pg(0, 1);
    EXTI->IMR = 1u << 0;
    SHOW("pr_unmask_after_edge", EXTI->PR);
    EXTI->PR = 0xFFFFFFFF;

    // software trigger
    EXTI->IMR = 0;
    EXTI->SWIER = 1u << 3;
    SHOW("swier_masked", EXTI->SWIER);
    SHOW("pr_swier_masked", EXTI->PR);
    EXTI->IMR = 1u << 3;
    EXTI->SWIER = 1u << 3;
    SHOW("swier_unmasked", EXTI->SWIER);
    SHOW("pr_swier_unmasked", EXTI->PR);
    EXTI->PR = 1u << 3;
    SHOW("swier_after_pr_clear", EXTI->SWIER);
    EXTI->SWIER = 0;
    EXTI->IMR = 0;

    // mode change: output high -> input with pull-down gives a falling edge
    EXTI->IMR = 1u << 0;
    EXTI->RTSR = 0;
    EXTI->FTSR = 1u << 0;
    pg(0, 1);
    EXTI->PR = 0xFFFFFFFF;
    GPIOG->PUPDR = (GPIOG->PUPDR & ~3u) | 2u;
    GPIOG->MODER &= ~3u;
    __DSB();
    for (volatile int i = 0; i < 100; i++) {
    }
    SHOW("pr_mode_change", EXTI->PR);
    GPIOG->MODER |= 1u;
    pg(0, 0);
    GPIOG->PUPDR &= ~3u;
    EXTI->PR = 0xFFFFFFFF;

    // other port selected: PG0 edges do not reach line 0
    SYSCFG->EXTICR[0] = 0x0; // PA0
    EXTI->RTSR = 1u << 0;
    pg(0, 1);
    SHOW("pr_other_port", EXTI->PR);
    pg(0, 0);
    SYSCFG->EXTICR[0] = 0x6;
    EXTI->PR = 0xFFFFFFFF;

    // interrupts: count and latency from the BSRR write to the ISR
    EXTI->IMR = (1u << 0) | (1u << 5);
    EXTI->RTSR = (1u << 0) | (1u << 5);
    EXTI->FTSR = 0;
    NVIC_EnableIRQ(EXTI0_IRQn);
    NVIC_EnableIRQ(EXTI9_5_IRQn);
    for (int line = 0; line <= 5; line += 5) {
        for (int k = 0; k < 3; k++) {
            exti_count = 0;
            __disable_irq();
            uint32_t t0 = DWT->CYCCNT;
            GPIOG->BSRR = 1u << line;
            __enable_irq();
            for (volatile int i = 0; i < 100; i++) {
            }
            printf("E irq%d count %lu pr %08lx latency %lu\n", line, (unsigned long)exti_count,
                   (unsigned long)exti_pr, (unsigned long)(exti_cyc - t0));
            pg(line, 0);
        }
    }
    // latency of a software trigger
    for (int k = 0; k < 3; k++) {
        exti_count = 0;
        __disable_irq();
        uint32_t t0 = DWT->CYCCNT;
        EXTI->SWIER = 1u << 0;
        __enable_irq();
        for (volatile int i = 0; i < 100; i++) {
        }
        printf("E swi0 count %lu latency %lu\n", (unsigned long)exti_count, (unsigned long)(exti_cyc - t0));
    }
    // cycles from the BSRR write until PR reads as set (interrupts off)
    NVIC_DisableIRQ(EXTI0_IRQn);
    for (int k = 0; k < 3; k++) {
        __disable_irq();
        uint32_t t0 = DWT->CYCCNT;
        GPIOG->BSRR = 1u << 0;
        while (!(EXTI->PR & 1u)) {
        }
        uint32_t t1 = DWT->CYCCNT;
        __enable_irq();
        EXTI->PR = 1u;
        pg(0, 0);
        printf("E poll0 %lu\n", (unsigned long)(t1 - t0));
    }
    NVIC_EnableIRQ(EXTI0_IRQn);
    // pending while the NVIC line is disabled, taken when it is enabled
    NVIC_DisableIRQ(EXTI0_IRQn);
    exti_count = 0;
    pg(0, 1);
    SHOW("count_nvic_off", exti_count);
    SHOW("pr_nvic_off", EXTI->PR);
    NVIC_EnableIRQ(EXTI0_IRQn);
    __DSB();
    __ISB();
    SHOW("count_nvic_on", exti_count);
    pg(0, 0);

    NVIC_DisableIRQ(EXTI0_IRQn);
    NVIC_DisableIRQ(EXTI9_5_IRQn);
    EXTI->IMR = 0;
    EXTI->RTSR = 0;
    EXTI->FTSR = 0;
    EXTI->PR = 0xFFFFFFFF;
    GPIOG->MODER &= ~((3u << 0) | (3u << 10));
}

/* ---- Timers: compare flags, CC interrupts, repetition counter, basic and
 * shared-interrupt timers. */
static volatile uint32_t tim3_cc, tim3_cnt_at_isr, tim1_up, tim10_up, tim1_cc, tim6_up;

void TIM3_IRQHandler(void) {
    tim3_cnt_at_isr = TIM3->CNT;
    TIM3->SR = ~TIM_SR_CC1IF;
    tim3_cc++;
}

void TIM1_UP_TIM10_IRQHandler(void) {
    if (TIM1->SR & TIM_SR_UIF) {
        TIM1->SR = ~TIM_SR_UIF;
        tim1_up++;
    }
    if (TIM10->SR & TIM_SR_UIF) {
        TIM10->SR = ~TIM_SR_UIF;
        tim10_up++;
    }
}

void TIM1_CC_IRQHandler(void) {
    TIM1->SR = ~TIM_SR_CC2IF;
    tim1_cc++;
}

void TIM6_DAC_IRQHandler(void) {
    TIM6->SR = 0;
    tim6_up++;
}

#define T(name, expr) printf("M %s %08lx\n", name, (unsigned long)(expr))

static void wait_ms(uint32_t ms) {
    uint32_t t0 = DWT->CYCCNT;
    while (DWT->CYCCNT - t0 < ms * 180000u) {
    }
}

static void cmd_tim(void) {
    CoreDebug->DEMCR |= CoreDebug_DEMCR_TRCENA_Msk;
    DWT->CTRL |= DWT_CTRL_CYCCNTENA_Msk;
    RCC->APB1ENR |= RCC_APB1ENR_TIM3EN | RCC_APB1ENR_TIM6EN | RCC_APB1ENR_TIM7EN;
    RCC->APB2ENR |= RCC_APB2ENR_TIM1EN | RCC_APB2ENR_TIM9EN | RCC_APB2ENR_TIM10EN;
    __DSB();
    T("tim1_arr_reset", TIM1->ARR);
    T("tim6_arr_reset", TIM6->ARR);
    TIM1->RCR = 0xFFFF;
    T("tim1_rcr_mask", TIM1->RCR);
    TIM1->RCR = 0;
    TIM1->DCR = 0xFFFF;
    T("tim1_dcr_mask", TIM1->DCR);
    TIM1->DCR = 0;
    TIM9->SR = 0;
    TIM9->EGR = 0x1E;
    T("tim9_egr_cc", TIM9->SR);
    TIM9->SR = 0;
    TIM6->DIER = 0xFFFF;
    T("tim6_dier_mask", TIM6->DIER);
    TIM6->DIER = 0;

    // TIM3 compare flags: 90 MHz, period 1000 ticks
    TIM3->CR1 = 0;
    TIM3->PSC = 0;
    TIM3->ARR = 999;
    TIM3->CCR1 = 300;
    TIM3->CCR2 = 2000;
    TIM3->CCR3 = 0;
    TIM3->CCR4 = 500;
    TIM3->CCMR1 = 0;
    TIM3->CCMR2 = 1u << 8; // CC4: input
    TIM3->EGR = 1;
    TIM3->SR = 0;
    __disable_irq();
    TIM3->CR1 = 1;
    while (TIM3->CNT < 200) {
    }
    uint32_t sr_200 = TIM3->SR;
    while (TIM3->CNT < 350) {
    }
    uint32_t sr_350 = TIM3->SR;
    while (!(TIM3->SR & TIM_SR_UIF)) {
    }
    uint32_t sr_wrap = TIM3->SR;
    __enable_irq();
    TIM3->CR1 = 0;
    T("tim3_sr_200", sr_200);
    T("tim3_sr_350", sr_350);
    T("tim3_sr_wrap", sr_wrap);

    // TIM3 CC1 interrupt: 1 kHz (PSC 89 -> 1 MHz, ARR 999), 20 ms
    TIM3->PSC = 89;
    TIM3->CCR1 = 250;
    TIM3->EGR = 1;
    TIM3->SR = 0;
    TIM3->DIER = TIM_DIER_CC1IE;
    tim3_cc = 0;
    NVIC_EnableIRQ(TIM3_IRQn);
    TIM3->CR1 = 1;
    wait_ms(20);
    TIM3->CR1 = 0;
    NVIC_DisableIRQ(TIM3_IRQn);
    TIM3->DIER = 0;
    T("tim3_cc_irqs_20ms", tim3_cc);
    T("tim3_cnt_at_isr", tim3_cnt_at_isr);

    // TIM1 repetition counter: 180 MHz / 18000 = 10 kHz, ARR 9 -> 1 ms
    // overflows, RCR 2 -> update every 3 ms; CC2 at 5 every overflow.
    TIM1->PSC = 17999;
    TIM1->ARR = 9;
    TIM1->RCR = 2;
    TIM1->CCR2 = 5;
    TIM1->EGR = 1;
    TIM1->SR = 0;
    TIM1->DIER = TIM_DIER_UIE | TIM_DIER_CC2IE;
    // TIM10 on the same interrupt line: 180 MHz / 18000, ARR 49 -> 5 ms
    TIM10->PSC = 17999;
    TIM10->ARR = 49;
    TIM10->EGR = 1;
    TIM10->SR = 0;
    TIM10->DIER = TIM_DIER_UIE;
    tim1_up = tim10_up = tim1_cc = 0;
    NVIC_EnableIRQ(TIM1_UP_TIM10_IRQn);
    NVIC_EnableIRQ(TIM1_CC_IRQn);
    TIM1->CR1 = 1;
    TIM10->CR1 = 1;
    wait_ms(31);
    TIM1->CR1 = 0;
    TIM10->CR1 = 0;
    NVIC_DisableIRQ(TIM1_UP_TIM10_IRQn);
    NVIC_DisableIRQ(TIM1_CC_IRQn);
    TIM1->DIER = 0;
    TIM10->DIER = 0;
    T("tim1_up_irqs_31ms", tim1_up);
    T("tim1_cc_irqs_31ms", tim1_cc);
    T("tim10_up_irqs_31ms", tim10_up);

    // TIM6: 90 MHz / 9000 = 10 kHz, ARR 19 -> 2 ms, one-pulse mode off
    TIM6->PSC = 8999;
    TIM6->ARR = 19;
    TIM6->EGR = 1;
    TIM6->SR = 0;
    TIM6->DIER = TIM_DIER_UIE;
    tim6_up = 0;
    NVIC_EnableIRQ(TIM6_DAC_IRQn);
    TIM6->CR1 = 1;
    wait_ms(21);
    TIM6->CR1 = 0;
    NVIC_DisableIRQ(TIM6_DAC_IRQn);
    TIM6->DIER = 0;
    T("tim6_up_irqs_21ms", tim6_up);

    // TIM7 one-pulse: stops after the first overflow
    TIM7->PSC = 8999;
    TIM7->ARR = 9;
    TIM7->EGR = 1;
    TIM7->SR = 0;
    TIM7->CR1 = TIM_CR1_OPM | TIM_CR1_CEN;
    wait_ms(5);
    T("tim7_opm_cr1", TIM7->CR1);
    T("tim7_opm_sr", TIM7->SR);
    T("tim7_opm_cnt", TIM7->CNT);
}

static void cmd_in(void) {
    printf("IN GPIOF %04lx GPIOG %04lx GPIOC %04lx\n", (unsigned long)(GPIOF->IDR & 0xFFFF),
           (unsigned long)(GPIOG->IDR & 0xFFFF), (unsigned long)(GPIOC->IDR & 0xFFFF));
}

static void execute(char *line) {
    char *arg = strchr(line, ' ');
    if (arg) {
        *arg++ = '\0';
    }
    unsigned long n = arg ? strtoul(arg, NULL, 16) : 0;
    if (!strcmp(line, "clear")) {
        GUI_clear((COLOR)n);
    } else if (!strcmp(line, "bars")) {
        cmd_bars();
    } else if (!strcmp(line, "orient")) {
        cmd_orient();
    } else if (!strcmp(line, "text")) {
        cmd_text();
    } else if (!strcmp(line, "geom")) {
        cmd_geom();
    } else if (!strcmp(line, "led")) {
        cmd_led((unsigned)n);
    } else if (!strcmp(line, "nled")) {
        cmd_nled((unsigned)n);
    } else if (!strcmp(line, "bl")) {
        TIM4->CCR4 = (uint32_t)strtoul(arg ? arg : "0", NULL, 10);
    } else if (!strcmp(line, "in")) {
        cmd_in();
    } else if (!strcmp(line, "pins")) {
        cmd_pins();
    } else if (!strcmp(line, "cyc")) {
        cmd_cyc();
    } else if (!strcmp(line, "div")) {
        cmd_div();
    } else if (!strcmp(line, "time")) {
        cmd_time();
    } else if (!strcmp(line, "rd")) {
        printf("R %08lx %08lx\n", n, (unsigned long)*(volatile uint32_t *)n);
    } else if (!strcmp(line, "wr")) {
        char *val = arg ? strchr(arg, ' ') : NULL;
        if (!val) {
            printf("? %s\n", line);
            return;
        }
        *(volatile uint32_t *)n = (uint32_t)strtoul(val, NULL, 16);
    } else if (!strcmp(line, "exti")) {
        cmd_exti();
    } else if (!strcmp(line, "tim")) {
        cmd_tim();
    } else {
        printf("? %s\n", line);
        return;
    }
    printf("ok %s\n", line);
}

int main(void) {
    initITSboard();
    initLCDTouch();
    initTimer();
    printf("ready\n");

    char line[64];
    unsigned int len = 0;
    while (1) {
        int c = stdin_getchar();
        if (c < 0) {
            continue;
        }
        if (c == '\r' || c == '\n') {
            line[len] = '\0';
            if (len > 0) {
                execute(line);
            }
            len = 0;
        } else if (len < sizeof(line) - 1) {
            line[len++] = (char)c;
        }
    }
}
