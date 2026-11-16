/**
 * Tutorial program of the GUI (loaded on its first start): shows every input
 * of the board on the display, mirrors the buttons on the LEDs and reports
 * each change on the virtual COM port.
 *   S0-S7 (PF0-7)        boxes on the display; pressed lights OUT0-7 (PD0-7)
 *   USER (PC13)          box on the display
 *   AUX0-5 (PG0-5)       open / GND / 3V3, told apart by switching the pull
 *   Analog pads          ADC1 IN13/12/10/9 (PC3, PC2, PC0, PB1), ADC3 IN8/7/6
 *                        (PF10, PF9, PF8) as bars; the last one changed is
 *                        also a bar graph on OUT8-15 (PE0-7)
 *   Touch                a dot where the display is touched
 *   Serial               a line typed in the console is answered
 * An open analog pad is told by the noise of 16 conversions. Text in Font16
 * and Font20 (Font12 is hard to read on the scaled-down display of the GUI).
 * Serial lines: "S3 pressed", "AUX2 (PG2): GND", "ADC3_8 (PF10): 2.10 V" (or
 * "open"; once the voltage has held still for SETTLE_MS, so that turning the
 * mouse wheel gives one line), "touch 120 200", "you typed: <line>".
 */
#include <stdio.h>
#include <string.h>
#include "stm32f4xx_hal.h"
#include "init.h"
#include "LCD_GUI.h"
#include "LCD_Touch.h"
#include "Driver_USART.h"

extern ARM_DRIVER_USART Driver_USART3;

#define BG BLACK
#define FG WHITE
#define DIM GRAY
#define ON YELLOW

/* touch area right of the inputs */
#define TOUCH_X0 310
#define TOUCH_Y0 34
#define TOUCH_X1 474
#define TOUCH_Y1 288
/* the line typed in the console, below everything */
#define TYPED_Y 296
/* a voltage is reported once it has held still this long */
#define SETTLE_MS 300

static const struct {
    const char *name;
    const char *pin;
    ADC_TypeDef *adc;
    uint32_t channel;
} pads[7] = {
    {"ADC123_13", "PC3", ADC1, 13}, {"ADC123_12", "PC2", ADC1, 12}, {"ADC123_10", "PC0", ADC1, 10},
    {"ADC12_9", "PB1", ADC1, 9},    {"ADC3_8", "PF10", ADC3, 8},    {"ADC3_7", "PF9", ADC3, 7},
    {"ADC3_6", "PF8", ADC3, 6},
};

static Coordinate at(int x, int y) {
    Coordinate c = {(POINT)x, (POINT)y};
    return c;
}

static void text(int x, int y, const char *s, sFONT *font, COLOR fg) {
    GUI_disStr(at(x, y), s, font, BG, fg);
}

static void box(int x0, int y0, int x1, int y1, COLOR color, int filled) {
    GUI_drawRectangle(at(x0, y0), at(x1, y1), BG, true, DOT_PIXEL_1X1);
    GUI_drawRectangle(at(x0, y0), at(x1, y1), color, filled, DOT_PIXEL_1X1);
}

/* ---- buttons */

static void draw_button(int n, int pressed) {
    /* S7 left ... S0 right, as on the board */
    int x = 6 + (7 - n) * 37;
    char label[3] = {'S', (char)('0' + n), 0};
    box(x, 34, x + 33, 58, pressed ? ON : DIM, pressed);
    GUI_disStr(at(x + 6, 38), label, &Font16, pressed ? ON : BG, pressed ? BLACK : FG);
}

static void draw_user(int pressed) {
    box(6, 66, 80, 90, pressed ? CYAN : DIM, pressed);
    GUI_disStr(at(21, 70), "USER", &Font16, pressed ? CYAN : BG, pressed ? BLACK : FG);
}

/* ---- AUX pads: a driven pad keeps its level whatever the pull, an open one follows it */

static int aux_level(int n) {
    uint32_t mask = 3u << (2 * n);
    GPIOG->PUPDR = (GPIOG->PUPDR & ~mask) | (2u << (2 * n)); /* pull-down */
    for (volatile int i = 0; i < 20; i++) {
    }
    int low_pull = (GPIOG->IDR >> n) & 1;
    GPIOG->PUPDR = (GPIOG->PUPDR & ~mask) | (1u << (2 * n)); /* pull-up (as initITSboard) */
    for (volatile int i = 0; i < 20; i++) {
    }
    int high_pull = (GPIOG->IDR >> n) & 1;
    if (low_pull == high_pull) {
        return low_pull; /* 0 = GND, 1 = 3V3 */
    }
    return 2; /* open */
}

static const char *const aux_names[3] = {"GND", "3V3", "open"};

static void draw_aux(int n, int level) {
    static const COLOR colors[3] = {BLUE, RED, DIM};
    int x = 6 + n * 49;
    box(x, 98, x + 46, 122, colors[level], level != 2);
    /* open: a dash (the heading says so), the word does not fit */
    if (level == 2) {
        GUI_disStr(at(x + 18, 102), "-", &Font16, BG, DIM);
    } else {
        GUI_disStr(at(x + 7, 102), aux_names[level], &Font16, colors[level], FG);
    }
}

/* ---- Analog pads */

static void init_adc(void) {
    __HAL_RCC_GPIOB_CLK_ENABLE();
    __HAL_RCC_GPIOC_CLK_ENABLE();
    __HAL_RCC_ADC1_CLK_ENABLE();
    __HAL_RCC_ADC3_CLK_ENABLE();
    GPIOC->MODER |= (3u << 0) | (3u << 4) | (3u << 6); /* PC0, PC2, PC3 analog */
    GPIOB->MODER |= 3u << 2;                           /* PB1 */
    GPIOF->MODER |= (3u << 16) | (3u << 18) | (3u << 20); /* PF8-10 */
    ADC->CCR = (ADC->CCR & ~ADC_CCR_ADCPRE) | ADC_CCR_ADCPRE_0; /* PCLK2 / 4 = 21 MHz */
    ADC1->SMPR1 = ADC3->SMPR1 = 0x07FFFFFF; /* 480 cycles on every channel */
    ADC1->SMPR2 = ADC3->SMPR2 = 0x3FFFFFFF;
    ADC1->CR2 = ADC3->CR2 = ADC_CR2_ADON;
}

static int convert(ADC_TypeDef *adc) {
    adc->SR = 0;
    adc->CR2 |= ADC_CR2_SWSTART;
    while (!(adc->SR & ADC_SR_EOC)) {
    }
    return (int)(adc->DR & 0xFFF);
}

/* Millivolts on pad n, or OPEN: an open pad floats around 1 V with a noise
 * of +-100 counts, an applied voltage gives a few counts. */
#define OPEN (-1)

static int adc_mv(int n) {
    ADC_TypeDef *adc = pads[n].adc;
    adc->SQR3 = pads[n].channel;
    int sum = 0, lo = 4095, hi = 0;
    for (int i = 0; i < 16; i++) {
        int c = convert(adc);
        sum += c;
        lo = c < lo ? c : lo;
        hi = c > hi ? c : hi;
    }
    if (hi - lo > 40) {
        return OPEN;
    }
    /* VDDA of the board (3.31 V, from VREFINT) */
    return (sum / 16) * 3310 / 4095;
}

static void volts(char *buf, size_t size, int mv) {
    if (mv == OPEN) {
        snprintf(buf, size, "open");
    } else {
        int cv = (mv + 5) / 10;
        snprintf(buf, size, "%d.%02d V", cv / 100, cv % 100);
    }
}

static void draw_pad(int n, int mv) {
    int y = 132 + n * 22;
    int w = mv == OPEN ? 0 : mv * 120 / 3300;
    char value[12];
    volts(value, sizeof value, mv);
    GUI_drawRectangle(at(56 + w, y), at(300, y + 16), BG, true, DOT_PIXEL_1X1);
    if (w > 0) {
        GUI_drawRectangle(at(56, y + 1), at(56 + w, y + 15), GREEN, true, DOT_PIXEL_1X1);
    }
    GUI_drawRectangle(at(56, y + 1), at(176, y + 15), DIM, false, DOT_PIXEL_1X1);
    text(184, y, value, &Font16, mv == OPEN ? DIM : FG);
}

/* Above the conversion noise (a step of the mouse wheel is 50 mV). */
static int differs(int a, int b) {
    return (a == OPEN) != (b == OPEN) || a - b > 20 || b - a > 20;
}

/* OUT8-15: bar graph of a voltage */
static void led_bar(int mv) {
    if (mv == OPEN) {
        mv = 0;
    }
    int lit = (mv * 8 + 1650) / 3300;
    GPIOE->ODR = (GPIOE->ODR & ~0xFFu) | ((1u << lit) - 1u);
}

/* ---- serial input without blocking: up to a line per Receive */

static uint8_t rx[64];
static uint32_t rx_seen;
static char line[64];
static uint32_t line_len;

static void poll_serial(void) {
    uint32_t n = Driver_USART3.GetRxCount();
    int restart = n >= sizeof rx;
    for (; rx_seen < n; rx_seen++) {
        char c = (char)rx[rx_seen];
        if (c == '\r' || c == '\n') {
            if (line_len > 0) {
                line[line_len] = 0;
                printf("you typed: %s\n", line);
                GUI_drawRectangle(at(0, TYPED_Y), at(479, 319), BG, true, DOT_PIXEL_1X1);
                text(6, TYPED_Y, "typed:", &Font16, DIM);
                /* what fits in a line of Font16 */
                line[36] = 0;
                text(83, TYPED_Y, line, &Font16, ON);
                line_len = 0;
            }
            restart = 1;
        } else if (line_len < sizeof line - 1) {
            line[line_len++] = c;
        }
    }
    if (restart) {
        Driver_USART3.Control(ARM_USART_ABORT_RECEIVE, 0);
        rx_seen = 0;
        Driver_USART3.Receive(rx, sizeof rx);
    }
}

static void draw_screen(void) {
    GUI_clear(BG);
    GUI_drawRectangle(at(0, 0), at(479, 27), BLUE, true, DOT_PIXEL_1X1);
    GUI_disStr(at(6, 4), "ITS-Board Tutorial", &Font20, BLUE, WHITE);
    text(96, 70, "AUX0..5 (- open)", &Font16, DIM);
    for (int n = 0; n < 7; n++) {
        text(6, 132 + n * 22, pads[n].pin, &Font16, FG);
    }
    box(TOUCH_X0, TOUCH_Y0, TOUCH_X1, TOUCH_Y1, DIM, 0);
    text(TOUCH_X0 + 27, TOUCH_Y1 - 22, "touch here", &Font16, DIM);
    text(6, TYPED_Y, "Type a line in the console", &Font16, DIM);
}

int main(void) {
    initITSboard();
    GUI_init(DEFAULT_BRIGHTNESS);
    TP_Init(false);

    /* USER: the Nucleo's pull-down holds it low until pressed */
    GPIO_InitTypeDef user = {GPIO_PIN_13, GPIO_MODE_INPUT, GPIO_NOPULL, GPIO_SPEED_FREQ_LOW, 0};
    __HAL_RCC_GPIOC_CLK_ENABLE();
    HAL_GPIO_Init(GPIOC, &user);
    init_adc();
    draw_screen();
    Driver_USART3.Receive(rx, sizeof rx);
    printf("Tutorial: every input of the board is shown on the display and reported here.\n");

    int buttons = -1, user_down = -1, aux[6], touching = 0;
    /* per analog pad: shown on the display, last reported, when it last moved */
    int mv[7], reported[7];
    uint32_t moved_at[7] = {0};
    for (int n = 0; n < 6; n++) {
        aux[n] = -1;
    }
    for (int n = 0; n < 7; n++) {
        mv[n] = -1000;
    }
    int first = 1;
    while (1) {
        /* buttons are active low */
        int now = ~GPIOF->IDR & 0xFF;
        GPIOD->ODR = (GPIOD->ODR & ~0xFFu) | (uint32_t)now;
        for (int n = 0; n < 8; n++) {
            int bit = 1 << n;
            if (buttons < 0 || (now ^ buttons) & bit) {
                draw_button(n, now & bit);
                if (buttons >= 0) {
                    printf("S%d %s\n", n, now & bit ? "pressed" : "released");
                }
            }
        }
        buttons = now;

        int u = (GPIOC->IDR >> 13) & 1;
        if (u != user_down) {
            draw_user(u);
            if (user_down >= 0) {
                printf("USER %s\n", u ? "pressed" : "released");
            }
            user_down = u;
        }

        for (int n = 0; n < 6; n++) {
            int level = aux_level(n);
            if (level != aux[n]) {
                draw_aux(n, level);
                if (aux[n] >= 0) {
                    printf("AUX%d (PG%d): %s\n", n, n, aux_names[level]);
                }
                aux[n] = level;
            }
        }

        for (int n = 0; n < 7; n++) {
            int v = adc_mv(n);
            if (differs(v, mv[n])) {
                draw_pad(n, v);
                if (!first) {
                    led_bar(v);
                }
                mv[n] = v;
                moved_at[n] = HAL_GetTick();
            }
            if (first) {
                reported[n] = v;
            } else if (differs(mv[n], reported[n]) && HAL_GetTick() - moved_at[n] >= SETTLE_MS) {
                char value[12];
                volts(value, sizeof value, mv[n]);
                printf("%s (%s): %s\n", pads[n].name, pads[n].pin, value);
                reported[n] = mv[n];
            }
        }

        if (TP_pressed()) {
            Coordinate c = TP_get();
            if (VALID_COORDINATE(c) && c.x > TOUCH_X0 + 3 && c.x < TOUCH_X1 - 3 && c.y > TOUCH_Y0 + 3 &&
                c.y < TOUCH_Y1 - 26) {
                GUI_drawPoint(c, ON, DOT_PIXEL_3X3, DOT_FILL_AROUND);
            }
            if (!touching && VALID_COORDINATE(c)) {
                printf("touch %d %d\n", c.x, c.y);
            }
            touching = 1;
        } else {
            touching = 0;
        }

        poll_serial();
        first = 0;
        HAL_Delay(10);
    }
}
