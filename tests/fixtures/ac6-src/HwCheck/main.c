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
 *   time            prints durations (TIM2 ticks, 90 per us) of library calls
 *   cyc             prints core cycles (DWT->CYCCNT) of instruction kernels
 *   div             prints UDIV/SDIV cycles for a range of operands
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
    } else if (!strcmp(line, "cyc")) {
        cmd_cyc();
    } else if (!strcmp(line, "div")) {
        cmd_div();
    } else if (!strcmp(line, "time")) {
        cmd_time();
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
