/**
 * Tutorial program of the GUI (loaded on its first start): leads through the
 * inputs of the board one step after another on the display. Each step shows
 * what to do, a map of the board with the part it is about, and what the
 * program reads; it moves on by itself once the input was used. Back / Skip /
 * Next at the bottom are touched. After the last step every input is shown
 * on one screen.
 *   S0-S7 (PF0-7)        pressed lights OUT0-7 (PD0-7)
 *   USER (PC13)
 *   AUX0-5 (PG0-5)       open / GND / 3V3, told apart by switching the pull
 *   Analog pads          ADC1 IN13/12/10/9 (PC3, PC2, PC0, PB1), ADC3 IN8/7/6
 *                        (PF10, PF9, PF8) as bars; the last one changed is
 *                        also a bar graph on OUT8-15 (PE0-7)
 *   Touch                a dot where the display is touched
 *   Serial               a line typed in the console is answered
 * An open analog pad is told by the noise of 16 conversions. Text in Font16
 * and Font20 (Font12 is hard to read on the scaled-down display of the GUI).
 * Serial lines, in every step: "Step 2/9: Buttons S0-S7", "S3 pressed",
 * "AUX2 (PG2): GND", "ADC3_8 (PF10): 2.10 V" (or "open"; once the voltage
 * has held still for SETTLE_MS, so that turning the mouse wheel gives one
 * line), "touch 120 200", "you typed: <line>".
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
#define PCB 0x0849   /* dark blue of the board */
#define PANEL 0x2965 /* the display module on the map */

/* a voltage is reported once it has held still this long */
#define SETTLE_MS 300
/* a done step stays this long, so that its effect can be seen */
#define ADVANCE_MS 1800
/* the highlighted part of the map blinks */
#define BLINK_MS 400

/* Layout of a step: title, text, the live part (left) and the map (right)
 * between TOP and BOTTOM, the buttons Back / Skip below. */
#define TEXT_Y 62
#define LINE_H 18
#define TOP 166
#define BOTTOM 286
#define LIVE_X1 300
#define NAV_Y 293
#define BACK_X0 6
#define BACK_X1 100
#define NEXT_X0 374
#define NEXT_X1 474

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

static void fill(int x0, int y0, int x1, int y1, COLOR color) {
    GUI_drawRectangle(at(x0, y0), at(x1, y1), color, true, DOT_PIXEL_1X1);
}

static void box(int x0, int y0, int x1, int y1, COLOR color, int filled) {
    fill(x0, y0, x1, y1, BG);
    GUI_drawRectangle(at(x0, y0), at(x1, y1), color, filled, DOT_PIXEL_1X1);
}

/* Text in Font16 from (x, y), broken between words at `cols` characters and
 * at '\n'. */
static void wrap(int x, int y, const char *s, int cols, COLOR fg) {
    char line[48];
    while (*s) {
        int n = 0, cut = 0;
        while (s[n] && s[n] != '\n' && n < cols) {
            if (s[n] == ' ') {
                cut = n;
            }
            n++;
        }
        if (s[n] && s[n] != '\n' && s[n] != ' ' && cut > 0) {
            n = cut;
        }
        memcpy(line, s, (size_t)n);
        line[n] = 0;
        text(x, y, line, &Font16, fg);
        y += LINE_H;
        s += n;
        if (*s == ' ' || *s == '\n') {
            s++;
        }
    }
}

/* ---- buttons */

static void draw_button(int x, int y, int n, int pressed) {
    char label[3] = {'S', (char)('0' + n), 0};
    box(x, y, x + 33, y + 24, pressed ? ON : DIM, pressed);
    GUI_disStr(at(x + 6, y + 4), label, &Font16, pressed ? ON : BG, pressed ? BLACK : FG);
}

static void draw_user(int x, int y, int pressed) {
    box(x, y, x + 74, y + 24, pressed ? CYAN : DIM, pressed);
    GUI_disStr(at(x + 15, y + 4), "USER", &Font16, pressed ? CYAN : BG, pressed ? BLACK : FG);
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

static void draw_aux(int x, int y, int level) {
    static const COLOR colors[3] = {BLUE, RED, DIM};
    box(x, y, x + 46, y + 24, colors[level], level != 2);
    /* open: a dash, the word does not fit */
    if (level == 2) {
        GUI_disStr(at(x + 18, y + 4), "-", &Font16, BG, DIM);
    } else {
        GUI_disStr(at(x + 7, y + 4), aux_names[level], &Font16, colors[level], FG);
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

/* A row of pad n: pin, bar and value, the bar from x + 50. */
static void draw_pad(int x, int y, int n, int mv) {
    int w = mv == OPEN ? 0 : mv * 120 / 3300;
    char value[12];
    volts(value, sizeof value, mv);
    text(x, y, pads[n].pin, &Font16, FG);
    fill(x + 50 + w, y, x + 248, y + 15, BG);
    if (w > 0) {
        fill(x + 50, y + 1, x + 50 + w, y + 15, GREEN);
    }
    GUI_drawRectangle(at(x + 50, y + 1), at(x + 170, y + 15), DIM, false, DOT_PIXEL_1X1);
    text(x + 178, y, value, &Font16, mv == OPEN ? DIM : FG);
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

/* 1 when a line was completed (in `line`) */
static int poll_serial(void) {
    int got = 0;
    uint32_t n = Driver_USART3.GetRxCount();
    int restart = n >= sizeof rx;
    for (; rx_seen < n && !got; rx_seen++) {
        char c = (char)rx[rx_seen];
        if (c == '\r' || c == '\n') {
            if (line_len > 0) {
                line[line_len] = 0;
                printf("you typed: %s\n", line);
                line_len = 0;
                got = 1;
            }
            restart = 1;
        } else if (line_len < sizeof line - 1) {
            line[line_len++] = c;
        }
    }
    if (restart && rx_seen >= n) {
        Driver_USART3.Control(ARM_USART_ABORT_RECEIVE, 0);
        rx_seen = 0;
        Driver_USART3.Receive(rx, sizeof rx);
    }
    return got;
}

/* ---- the inputs, read once per pass of the main loop */

static struct {
    int buttons; /* bit n: Sn pressed */
    int user;
    int aux[6];
    int mv[7];
    int reported[7];
    uint32_t moved_at[7];
    int touching;
    Coordinate touch;
} in;

/* What changed in this pass. */
static struct {
    int buttons; /* bits that changed */
    int user;
    int aux; /* bit n: AUXn */
    int pads; /* bit n: pad n */
    int touched; /* a touch began */
    int typed;
} ch;

static void read_inputs(int first) {
    memset(&ch, 0, sizeof ch);
    /* buttons are active low */
    int now = ~GPIOF->IDR & 0xFF;
    GPIOD->ODR = (GPIOD->ODR & ~0xFFu) | (uint32_t)now;
    ch.buttons = first ? 0xFF : now ^ in.buttons;
    for (int n = 0; n < 8; n++) {
        if (!first && ch.buttons & (1 << n)) {
            printf("S%d %s\n", n, now & (1 << n) ? "pressed" : "released");
        }
    }
    in.buttons = now;

    int u = (GPIOC->IDR >> 13) & 1;
    if (first || u != in.user) {
        ch.user = 1;
        if (!first) {
            printf("USER %s\n", u ? "pressed" : "released");
        }
        in.user = u;
    }

    for (int n = 0; n < 6; n++) {
        int level = aux_level(n);
        if (first || level != in.aux[n]) {
            ch.aux |= 1 << n;
            if (!first) {
                printf("AUX%d (PG%d): %s\n", n, n, aux_names[level]);
            }
            in.aux[n] = level;
        }
    }

    for (int n = 0; n < 7; n++) {
        int v = adc_mv(n);
        if (first || differs(v, in.mv[n])) {
            ch.pads |= 1 << n;
            if (!first) {
                led_bar(v);
            }
            in.mv[n] = v;
            in.moved_at[n] = HAL_GetTick();
        }
        if (first) {
            in.reported[n] = v;
        } else if (differs(in.mv[n], in.reported[n]) && HAL_GetTick() - in.moved_at[n] >= SETTLE_MS) {
            char value[12];
            volts(value, sizeof value, in.mv[n]);
            printf("%s (%s): %s\n", pads[n].name, pads[n].pin, value);
            in.reported[n] = in.mv[n];
        }
    }

    if (TP_pressed()) {
        Coordinate c = TP_get();
        if (VALID_COORDINATE(c)) {
            if (!in.touching) {
                printf("touch %d %d\n", c.x, c.y);
                ch.touched = 1;
            }
            in.touch = c;
            in.touching = 1;
        }
    } else {
        in.touching = 0;
    }

    ch.typed = poll_serial();
}

/* ---- map of the board (board units of 0.1 mm as the GUI's drawing, scaled
 * by 16/100): where the part of a step is, seen from the display */

#define MAP_X 308
#define MAP_Y 172

enum part { P_NONE, P_DISPLAY, P_BUTTONS, P_USER, P_AUX, P_ANALOG, P_CONSOLE };

static int mx(int x) {
    return MAP_X + x * 16 / 100;
}

static int my(int y) {
    return MAP_Y + y * 16 / 100;
}

static void map_part(enum part p, COLOR c) {
    static const int button_x[8] = {911, 879, 847, 814, 782, 750, 718, 686};
    static const int aux_y[6] = {286, 317, 349, 380, 412, 449};
    static const int analog_x[8] = {440, 503, 535, 566, 598, 629, 661, 692};
    switch (p) {
    case P_DISPLAY:
        fill(mx(500), my(82), mx(892), my(335), c == ON ? ON : PANEL);
        GUI_drawRectangle(at(mx(532), my(87)), at(mx(879), my(318)), c == ON ? BLACK : DIM, false,
                          DOT_PIXEL_1X1);
        break;
    case P_BUTTONS:
        for (int n = 0; n < 8; n++) {
            fill(mx(button_x[n]) - 2, my(620) - 2, mx(button_x[n]) + 2, my(620) + 2, c);
        }
        break;
    case P_USER:
        GUI_drawCircle(at(mx(321), my(92)), 3, c, true, DOT_PIXEL_1X1);
        break;
    case P_AUX:
        for (int n = 0; n < 6; n++) {
            fill(mx(946) - 2, my(aux_y[n]) - 2, mx(946) + 2, my(aux_y[n]) + 2, c);
        }
        break;
    case P_ANALOG:
        /* DAC1 and the ADC pads */
        for (int n = 0; n < 8; n++) {
            fill(mx(analog_x[n]) - 2, my(449) - 2, mx(analog_x[n]) + 2, my(449) + 2, c);
        }
        break;
    case P_CONSOLE:
        /* below the board, as the GUI shows it by default */
        fill(mx(0), my(672), mx(1010), my(700), c);
        break;
    case P_NONE:
        break;
    }
}

/* A frame around part p (the console is a bar: it blinks itself). */
static void map_frame(enum part p, int on) {
    static const short bounds[][4] = {
        [P_DISPLAY] = {480, 62, 912, 355}, [P_BUTTONS] = {660, 596, 937, 644},
        [P_USER] = {290, 61, 352, 123},    [P_AUX] = {920, 260, 972, 475},
        [P_ANALOG] = {414, 423, 718, 475},
    };
    if (p == P_CONSOLE) {
        map_part(p, on ? ON : BG);
    } else if (p != P_NONE) {
        GUI_drawRectangle(at(mx(bounds[p][0]), my(bounds[p][1])), at(mx(bounds[p][2]), my(bounds[p][3])),
                          on ? ON : PCB, false, DOT_PIXEL_2X2);
    }
}

static void draw_map(enum part highlight) {
    fill(mx(0) - 2, MAP_Y - 2, 479, BOTTOM, BG);
    fill(mx(0), my(0), mx(1010), my(658), PCB);
    /* the Nucleo */
    GUI_drawRectangle(at(mx(302), my(67)), at(mx(498), my(350)), DIM, false, DOT_PIXEL_1X1);
    for (enum part p = P_DISPLAY; p <= P_CONSOLE; p++) {
        map_part(p, p == highlight ? ON : p == P_CONSOLE ? BG : DIM);
    }
    map_frame(highlight, 1);
}

/* ---- the steps */

enum goal { G_NONE, G_BUTTON, G_TWO, G_USER, G_AUX, G_ANALOG, G_TOUCH, G_SERIAL };

static const struct step {
    const char *title;
    const char *text;
    enum part part;
    enum goal goal;
    const char *done;
} steps[] = {
    {"Welcome",
     "This program leads through the inputs of the board. Each step moves on by itself once you "
     "have used the input; the map shows where it is.\nThe display is a touch screen: touch "
     "(click) Next.",
     P_DISPLAY, G_NONE, ""},
    {"Buttons S0-S7",
     "Press a button below the display (in the emulator: click it or hold a key 0-7). Its red LED "
     "IN0-7 reads the input (PF0-7, active low) and goes dark; the program lights OUT0-7.",
     P_BUTTONS, G_BUTTON, "The program saw it."},
    {"Two buttons at once",
     "Hold two buttons at the same time. In the emulator a right click keeps a button pressed "
     "(yellow ring) until the next right click.",
     P_BUTTONS, G_TWO, "Two held at once."},
    {"USER button",
     "Press the blue USER button of the Nucleo, left of the display (PC13). Unlike S0-S7 it reads "
     "high while pressed.",
     P_USER, G_USER, "USER read high."},
    {"Digital inputs AUX0-5",
     "Connect a pad AUX0-5 (PG0-5) to 3V3. Emulator: a click ties a pad to GND, the next to 3V3, a "
     "third leaves it open. The program tells them apart by switching the pull.",
     P_AUX, G_AUX, "A pad at 3V3."},
    {"Analog inputs",
     "Put 2 V or more on an Analog pad (ADC1, ADC3). Emulator: mouse wheel over a pad (0.05 V a "
     "step), double click 3.3 V, right click a slider. OUT8-15 show the last change.",
     P_ANALOG, G_ANALOG, "2 V or more read."},
    {"Touch screen",
     "Touch (click or drag) in the box: the program draws where. Emulator: a right click on the "
     "display shows it alone and enlarged, Esc returns.",
     P_DISPLAY, G_TOUCH, "Touch seen."},
    {"Serial console",
     "printf writes to the virtual COM port (USART3, 115200 baud). Type a line in the serial "
     "console (emulator: below or right of the board, View menu) and press Enter: the program "
     "answers.",
     P_CONSOLE, G_SERIAL, "Line received."},
    {"Your own program",
     "Emulator: open your .axf with Open... (Ctrl+O, Cmd+O on a Mac) or drop it onto the window; it "
     "is loaded again whenever you rebuild it. F1 lists all controls, Help > Tutorial starts this "
     "again.\n\nFinish shows every input on one screen.",
     P_NONE, G_NONE, ""},
};

#define STEPS ((int)(sizeof steps / sizeof steps[0]))

/* touch box of the touch step */
#define TOUCH_X0 6
#define TOUCH_X1 (LIVE_X1 - 4)

static void draw_live(int step, int all) {
    switch (steps[step].goal) {
    case G_BUTTON:
    case G_TWO:
        /* S7 left ... S0 right, as on the board */
        for (int n = 0; n < 8; n++) {
            if (all || ch.buttons & (1 << n)) {
                draw_button(6 + (7 - n) * 37, TOP + 30, n, in.buttons & (1 << n));
            }
        }
        break;
    case G_USER:
        if (all || ch.user) {
            draw_user(6, TOP + 30, in.user);
        }
        break;
    case G_AUX:
        for (int n = 0; n < 6; n++) {
            if (all) {
                char label[5] = {'A', 'U', 'X', (char)('0' + n), 0};
                text(6 + n * 49 + 1, TOP + 20, label, &Font16, FG);
            }
            if (all || ch.aux & (1 << n)) {
                draw_aux(6 + n * 49, TOP + 40, in.aux[n]);
            }
        }
        break;
    case G_ANALOG:
        for (int n = 0; n < 7; n++) {
            if (all || ch.pads & (1 << n)) {
                draw_pad(6, TOP + n * 17, n, in.mv[n]);
            }
        }
        break;
    case G_TOUCH:
        if (all) {
            box(TOUCH_X0, TOP, TOUCH_X1, BOTTOM, DIM, 0);
            text(TOUCH_X0 + 92, BOTTOM - 22, "touch here", &Font16, DIM);
        }
        break;
    case G_SERIAL:
        if (all) {
            text(6, TOP + 20, "typed:", &Font16, DIM);
        }
        if (ch.typed) {
            fill(6, TOP + 44, LIVE_X1, TOP + 44 + 2 * LINE_H, BG);
            line[2 * 26] = 0;
            wrap(6, TOP + 44, line, 26, ON);
        }
        break;
    case G_NONE:
        break;
    }
}

static int goal_met(enum goal g) {
    switch (g) {
    case G_BUTTON:
        return (ch.buttons & in.buttons) != 0;
    case G_TWO: {
        int b = in.buttons;
        return (ch.buttons & b) && (b & (b - 1));
    }
    case G_USER:
        return ch.user && in.user;
    case G_AUX:
        for (int n = 0; n < 6; n++) {
            if (ch.aux & (1 << n) && in.aux[n] == 1) {
                return 1;
            }
        }
        return 0;
    case G_ANALOG:
        for (int n = 0; n < 7; n++) {
            if (ch.pads & (1 << n) && in.mv[n] >= 1990) {
                return 1;
            }
        }
        return 0;
    case G_TOUCH:
        return in.touching && in.touch.x > TOUCH_X0 && in.touch.x < TOUCH_X1 && in.touch.y > TOP &&
               in.touch.y < BOTTOM;
    case G_SERIAL:
        return ch.typed;
    case G_NONE:
        break;
    }
    return 0;
}

static void nav_button(int x0, int x1, const char *label, int primary) {
    int w = (int)strlen(label) * 11;
    box(x0, NAV_Y, x1, 318, primary ? BLUE : DIM, primary);
    GUI_disStr(at((x0 + x1 - w) / 2, NAV_Y + 5), label, &Font16, primary ? BLUE : BG, FG);
}

static void draw_nav(int step, int met) {
    fill(0, NAV_Y, 479, 319, BG);
    if (step > 0) {
        nav_button(BACK_X0, BACK_X1, "< Back", 0);
    }
    int last = step + 1 == STEPS;
    int waits = steps[step].goal != G_NONE && !met;
    nav_button(NEXT_X0, NEXT_X1, last ? "Finish" : waits ? "Skip >" : "Next >", !waits);
    if (met) {
        /* a check mark and what was seen */
        GUI_drawLine(at(116, NAV_Y + 12), at(121, NAV_Y + 18), GREEN, BG, true, DOT_PIXEL_2X2);
        GUI_drawLine(at(121, NAV_Y + 18), at(132, NAV_Y + 5), GREEN, BG, true, DOT_PIXEL_2X2);
        text(140, NAV_Y + 5, steps[step].done, &Font16, GREEN);
    }
}

/* the header with a dot per step (done, current, to come) */
static void draw_header(int step) {
    fill(0, 0, 479, 27, BLUE);
    GUI_disStr(at(6, 4), "ITS-Board Tutorial", &Font20, BLUE, WHITE);
    for (int n = 0; n < STEPS; n++) {
        Coordinate c = at(300 + n * 14, 14);
        if (n == step) {
            GUI_drawCircle(c, 5, ON, true, DOT_PIXEL_1X1);
        } else {
            GUI_drawCircle(c, 3, n < step ? ON : WHITE, n < step, DOT_PIXEL_1X1);
        }
    }
    char count[8];
    snprintf(count, sizeof count, "%d/%d", step + 1, STEPS);
    GUI_disStr(at(430, 6), count, &Font16, BLUE, WHITE);
}

static void draw_step(int step) {
    GUI_clear(BG);
    draw_header(step);
    text(6, 36, steps[step].title, &Font20, ON);
    wrap(6, TEXT_Y, steps[step].text, 42, FG);
    if (steps[step].part != P_NONE) {
        draw_map(steps[step].part);
    }
    if (step == 0) {
        /* the legend of the map: an arrow to the display */
        int y = TOP + 48, tip_x = mx(500) - 3, tip_y = my(210);
        text(6, TOP + 20, "On the map:", &Font16, DIM);
        text(6, TOP + 40, "this display", &Font16, ON);
        GUI_drawLine(at(148, y), at(tip_x, tip_y), ON, BG, true, DOT_PIXEL_2X2);
        GUI_drawLine(at(tip_x - 9, tip_y - 4), at(tip_x, tip_y), ON, BG, true, DOT_PIXEL_2X2);
        GUI_drawLine(at(tip_x - 7, tip_y + 6), at(tip_x, tip_y), ON, BG, true, DOT_PIXEL_2X2);
    }
    draw_live(step, 1);
    draw_nav(step, 0);
}

/* ---- after the tutorial: every input on one screen */

#define ALL_TOUCH_X0 310
#define ALL_TOUCH_Y0 34
#define ALL_TOUCH_X1 474
#define ALL_TOUCH_Y1 288
#define TYPED_Y 296

static void draw_all(int all) {
    if (all) {
        GUI_clear(BG);
        fill(0, 0, 479, 27, BLUE);
        GUI_disStr(at(6, 4), "ITS-Board inputs", &Font20, BLUE, WHITE);
        text(96, 70, "AUX0..5 (- open)", &Font16, DIM);
        box(ALL_TOUCH_X0, ALL_TOUCH_Y0, ALL_TOUCH_X1, ALL_TOUCH_Y1, DIM, 0);
        text(ALL_TOUCH_X0 + 27, ALL_TOUCH_Y1 - 22, "touch here", &Font16, DIM);
        text(6, TYPED_Y, "Type a line in the console", &Font16, DIM);
    }
    for (int n = 0; n < 8; n++) {
        if (all || ch.buttons & (1 << n)) {
            draw_button(6 + (7 - n) * 37, 34, n, in.buttons & (1 << n));
        }
    }
    if (all || ch.user) {
        draw_user(6, 66, in.user);
    }
    for (int n = 0; n < 6; n++) {
        if (all || ch.aux & (1 << n)) {
            draw_aux(6 + n * 49, 98, in.aux[n]);
        }
    }
    for (int n = 0; n < 7; n++) {
        if (all || ch.pads & (1 << n)) {
            draw_pad(6, 132 + n * 22, n, in.mv[n]);
        }
    }
    if (in.touching && in.touch.x > ALL_TOUCH_X0 + 3 && in.touch.x < ALL_TOUCH_X1 - 3 &&
        in.touch.y > ALL_TOUCH_Y0 + 3 && in.touch.y < ALL_TOUCH_Y1 - 26) {
        GUI_drawPoint(in.touch, ON, DOT_PIXEL_3X3, DOT_FILL_AROUND);
    }
    if (ch.typed) {
        fill(0, TYPED_Y, 479, 319, BG);
        text(6, TYPED_Y, "typed:", &Font16, DIM);
        /* what fits in a line of Font16 */
        line[36] = 0;
        text(83, TYPED_Y, line, &Font16, ON);
    }
}

static int on_nav(int x0, int x1) {
    return ch.touched && in.touch.x >= x0 && in.touch.x <= x1 && in.touch.y >= NAV_Y;
}

static void go_to(int step) {
    printf("Step %d/%d: %s\n", step + 1, STEPS, steps[step].title);
    draw_step(step);
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
    Driver_USART3.Receive(rx, sizeof rx);
    printf("Tutorial: follow the steps on the display; every input is reported here.\n");
    read_inputs(1);

    /* step STEPS: the tutorial is over, every input on one screen */
    int step = 0, met = 0, blink = 0;
    uint32_t met_at = 0, blinked_at = HAL_GetTick();
    go_to(step);
    while (1) {
        read_inputs(0);
        if (step == STEPS) {
            draw_all(0);
            HAL_Delay(10);
            continue;
        }
        draw_live(step, 0);
        if (steps[step].goal == G_TOUCH && in.touching && goal_met(G_TOUCH)) {
            GUI_drawPoint(in.touch, ON, DOT_PIXEL_3X3, DOT_FILL_AROUND);
        }
        if (!met && goal_met(steps[step].goal)) {
            met = 1;
            met_at = HAL_GetTick();
            draw_nav(step, 1);
        }
        int next = on_nav(NEXT_X0, NEXT_X1) || (met && HAL_GetTick() - met_at >= ADVANCE_MS);
        int back = step > 0 && on_nav(BACK_X0, BACK_X1);
        if (next || back) {
            step += next ? 1 : -1;
            met = 0;
            if (step == STEPS) {
                printf("Tutorial done: every input on one screen.\n");
                draw_all(1);
            } else {
                go_to(step);
            }
        } else if (steps[step].part != P_NONE && HAL_GetTick() - blinked_at >= BLINK_MS) {
            blink = !blink;
            blinked_at = HAL_GetTick();
            map_frame(steps[step].part, !blink);
        }
        HAL_Delay(10);
    }
}
