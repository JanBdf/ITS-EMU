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
