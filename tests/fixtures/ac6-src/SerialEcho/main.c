/**
 * Emulator test program: line echo over the virtual COM port (USART3).
 * Uses the ITS_BRD_LIB stdio redirection (Keil Driver_USART, interrupt driven).
 */
#include <stdio.h>
#include "init.h"
#include "lcd.h"

extern int stdin_getchar(void);

int main(void) {
    initITSboard();
    printf("ready\n");

    char line[64];
    unsigned int len = 0;
    unsigned int lines = 0;
    while (1) {
        int c = stdin_getchar();
        if (c < 0) {
            continue;
        }
        if (c == '\r' || c == '\n') {
            line[len] = '\0';
            lines++;
            printf("echo %u: %s (%u chars)\n", lines, line, len);
            len = 0;
        } else if (len < sizeof(line) - 1) {
            line[len++] = (char)c;
        }
    }
}
