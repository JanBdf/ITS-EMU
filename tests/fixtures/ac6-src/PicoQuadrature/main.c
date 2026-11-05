/**
 * Emulator test program: quadrature decoder for the square wave generator
 * on a Raspberry Pi Pico (A0 -> PF0 = channel A, A5 -> PF1 = channel B).
 *
 * EXTI0/EXTI1 (both edges of PF0/PF1) count phase changes: (B A) 00 -> 10 -> 11 -> 01 is
 * one direction (+1 each), the reverse the other. Both signals changing at
 * once is an error (counted, the step is ignored). 1200 phase changes are a
 * full turn (0.3 degree each).
 *
 * Display (every 100 ms): phase changes, angle, angular speed, direction, errors.
 * LEDs: OUT0-7 phase changes (low byte), OUT13 backward, OUT14 forward,
 * OUT15 error seen. S6 resets the counters (S0/S1 are the signal inputs).
 * Every 500 ms (while something changed) a line goes to the virtual COM port:
 *   count C angle A speed V dir D errors E
 */
#include <stdio.h>
#include "stm32f4xx_hal.h"
#include "init.h"
#include "lcd.h"
#include "timer.h"

#define REPORT_US 500000U
#define SPEED_WINDOW_US 250000U
#define DISPLAY_US 100000U

/* position (0..3) of the levels (B << 1 | A) in the forward cycle */
static const int position[4] = {0, 3, 1, 2};

static int read_levels(void) {
    return (int)(GPIOF->IDR & 3U);
}

static volatile int count, errors, dir;
static int last;

static void decode(void) {
    int now = read_levels();
    if (now == last) {
        return;
    }
    int step = (position[now] - position[last] + 4) % 4;
    if (step == 1) {
        count++;
        dir = 1;
    } else if (step == 3) {
        count--;
        dir = -1;
    } else {
        errors++;
    }
    last = now;
}

void EXTI0_IRQHandler(void) {
    EXTI->PR = 1U << 0;
    decode();
}

void EXTI1_IRQHandler(void) {
    EXTI->PR = 1U << 1;
    decode();
}

static void init_exti(void) {
    RCC->APB2ENR |= RCC_APB2ENR_SYSCFGEN;
    /* EXTI0, EXTI1: port F */
    SYSCFG->EXTICR[0] = (SYSCFG->EXTICR[0] & ~0xFFU) | 0x55U;
    EXTI->RTSR |= 3U;
    EXTI->FTSR |= 3U;
    EXTI->PR = 3U;
    EXTI->IMR |= 3U;
    NVIC_EnableIRQ(EXTI0_IRQn);
    NVIC_EnableIRQ(EXTI1_IRQn);
}

static void show_leds(int count, int dir, int error) {
    uint32_t low = (uint32_t)count & 0xFFU;
    GPIOD->BSRR = low | ((~low & 0xFFU) << 16);
    uint32_t e = (dir < 0 ? 1U << 5 : 0U) | (dir > 0 ? 1U << 6 : 0U) | (error ? 1U << 7 : 0U);
    GPIOE->BSRR = e | ((~e & 0xE0U) << 16);
}

static void show_value(int row, const char *text) {
    lcdGotoXY(14, row);
    lcdPrintS((char *)text);
    lcdPrintS("          ");
}

int main(void) {
    initITSboard();
    initLCDTouch();
    initTimer();
    lcdSetFont(16);
    lcdGotoXY(0, 0);
    lcdPrintS("Phasen:");
    lcdGotoXY(0, 1);
    lcdPrintS("Winkel:");
    lcdGotoXY(0, 2);
    lcdPrintS("Geschw.:");
    lcdGotoXY(0, 3);
    lcdPrintS("Richtung:");
    lcdGotoXY(0, 4);
    lcdPrintS("Fehler:");
    printf("PicoQuadrature ready\n");

    last = read_levels();
    init_exti();
    int shown_count = -1, shown_errors = -1, shown_dir = 2;
    int window_count = 0;
    double speed = 0.0, shown_speed = -1.0;
    uint32_t window_start = getTimeStamp();
    uint32_t last_report = window_start;
    uint32_t last_display = window_start;
    int reported = 1;

    while (1) {
        uint32_t t = getTimeStamp();
        uint32_t window = (t - window_start) / TICKS_PER_US;
        if (window >= SPEED_WINDOW_US) {
            speed = (count - window_count) * 0.3 * 1e6 / window;
            window_count = count;
            window_start = t;
        }

        if (!(GPIOF->IDR & (1U << 6))) { /* S6: reset */
            __disable_irq();
            count = errors = dir = 0;
            __enable_irq();
            window_count = 0;
            speed = 0.0;
        }

        if ((t - last_display) / TICKS_PER_US < DISPLAY_US) {
            continue;
        }
        last_display = t;
        if (count != shown_count || errors != shown_errors || dir != shown_dir || speed != shown_speed) {
            char buf[24];
            snprintf(buf, sizeof buf, "%d", count);
            show_value(0, buf);
            snprintf(buf, sizeof buf, "%.1f", count * 0.3);
            show_value(1, buf);
            snprintf(buf, sizeof buf, "%.1f/s", speed);
            show_value(2, buf);
            show_value(3, dir > 0 ? "vorwaerts" : dir < 0 ? "rueckwaerts" : "-");
            snprintf(buf, sizeof buf, "%d", errors);
            show_value(4, buf);
            show_leds(count, dir, errors != 0);
            shown_count = count;
            shown_errors = errors;
            shown_dir = dir;
            shown_speed = speed;
            reported = 0;
        }

        if (!reported && (t - last_report) / TICKS_PER_US >= REPORT_US) {
            printf("count %d angle %.1f speed %.1f dir %d errors %d\n", count, count * 0.3, speed, dir, errors);
            last_report = t;
            reported = 1;
        }
    }
}
