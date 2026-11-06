/**
 * Emulator test program: an LM75 temperature sensor at 0x48 on the I2C
 * header (I2C1, SCL PB8, SDA PB9) read and written with the HAL, polled,
 * with interrupts and with DMA (DMA1 stream 0 / 6, channel 1). Lines on the
 * virtual COM port:
 *   L <what> <value>
 * then every 100 ms the temperature register: L t <raw>
 */
#include <stdio.h>
#include "stm32f4xx_hal.h"
#include "init.h"

#define LM75 (0x48 << 1)

static I2C_HandleTypeDef hi2c;
static DMA_HandleTypeDef hdma_rx, hdma_tx;
static volatile int done;

void I2C1_EV_IRQHandler(void) {
    HAL_I2C_EV_IRQHandler(&hi2c);
}

void I2C1_ER_IRQHandler(void) {
    HAL_I2C_ER_IRQHandler(&hi2c);
}

void DMA1_Stream0_IRQHandler(void) {
    HAL_DMA_IRQHandler(&hdma_rx);
}

void DMA1_Stream6_IRQHandler(void) {
    HAL_DMA_IRQHandler(&hdma_tx);
}

void HAL_I2C_MemRxCpltCallback(I2C_HandleTypeDef *h) {
    (void)h;
    done = 1;
}

void HAL_I2C_MemTxCpltCallback(I2C_HandleTypeDef *h) {
    (void)h;
    done = 1;
}

void HAL_I2C_ErrorCallback(I2C_HandleTypeDef *h) {
    printf("L error %lu\n", (unsigned long)h->ErrorCode);
    done = 1;
}

static int wait_done(void) {
    uint32_t t0 = HAL_GetTick();
    while (!done && HAL_GetTick() - t0 < 100) {
    }
    int ok = done;
    done = 0;
    while (HAL_I2C_GetState(&hi2c) != HAL_I2C_STATE_READY && HAL_GetTick() - t0 < 100) {
    }
    return ok;
}

static void setup(void) {
    __HAL_RCC_GPIOB_CLK_ENABLE();
    __HAL_RCC_I2C1_CLK_ENABLE();
    __HAL_RCC_DMA1_CLK_ENABLE();
    GPIO_InitTypeDef pins = {GPIO_PIN_8 | GPIO_PIN_9, GPIO_MODE_AF_OD, GPIO_PULLUP, GPIO_SPEED_FREQ_HIGH,
                             GPIO_AF4_I2C1};
    HAL_GPIO_Init(GPIOB, &pins);
    hi2c.Instance = I2C1;
    hi2c.Init.ClockSpeed = 100000;
    hi2c.Init.DutyCycle = I2C_DUTYCYCLE_2;
    hi2c.Init.AddressingMode = I2C_ADDRESSINGMODE_7BIT;
    HAL_I2C_Init(&hi2c);

    hdma_rx.Instance = DMA1_Stream0;
    hdma_rx.Init.Channel = DMA_CHANNEL_1;
    hdma_rx.Init.Direction = DMA_PERIPH_TO_MEMORY;
    hdma_rx.Init.MemInc = DMA_MINC_ENABLE;
    hdma_rx.Init.FIFOMode = DMA_FIFOMODE_DISABLE;
    HAL_DMA_Init(&hdma_rx);
    __HAL_LINKDMA(&hi2c, hdmarx, hdma_rx);
    hdma_tx.Instance = DMA1_Stream6;
    hdma_tx.Init.Channel = DMA_CHANNEL_1;
    hdma_tx.Init.Direction = DMA_MEMORY_TO_PERIPH;
    hdma_tx.Init.MemInc = DMA_MINC_ENABLE;
    hdma_tx.Init.FIFOMode = DMA_FIFOMODE_DISABLE;
    HAL_DMA_Init(&hdma_tx);
    __HAL_LINKDMA(&hi2c, hdmatx, hdma_tx);
    HAL_NVIC_EnableIRQ(I2C1_EV_IRQn);
    HAL_NVIC_EnableIRQ(I2C1_ER_IRQn);
    HAL_NVIC_EnableIRQ(DMA1_Stream0_IRQn);
    HAL_NVIC_EnableIRQ(DMA1_Stream6_IRQn);
}

static unsigned be16(const uint8_t *b) {
    return (unsigned)b[0] << 8 | b[1];
}

int main(void) {
    initITSboard();
    setup();
    uint8_t b[4] = {0};

    printf("L ready %d\n", HAL_I2C_IsDeviceReady(&hi2c, LM75, 2, 10) == HAL_OK);
    printf("L absent %d\n", HAL_I2C_IsDeviceReady(&hi2c, 0x49 << 1, 1, 10) == HAL_OK);
    HAL_StatusTypeDef st = HAL_I2C_Mem_Read(&hi2c, LM75, 0, I2C_MEMADD_SIZE_8BIT, b, 2, 10);
    printf("L poll %d %04x err %lu\n", st, be16(b), (unsigned long)hi2c.ErrorCode);
    uint8_t tos[2] = {0x55, 0x00};
    st = HAL_I2C_Mem_Write(&hi2c, LM75, 3, I2C_MEMADD_SIZE_8BIT, tos, 2, 10);
    b[0] = b[1] = 0;
    HAL_I2C_Mem_Read(&hi2c, LM75, 3, I2C_MEMADD_SIZE_8BIT, b, 2, 10);
    printf("L tos %d %04x\n", st, be16(b));

    b[0] = b[1] = 0;
    st = HAL_I2C_Mem_Read_IT(&hi2c, LM75, 3, I2C_MEMADD_SIZE_8BIT, b, 2);
    printf("L it %d %d %04x\n", st, wait_done(), be16(b));

    // DMA: T_hyst written, then four bytes read (the register repeats)
    uint8_t hyst[2] = {0x50, 0x80};
    st = HAL_I2C_Mem_Write_DMA(&hi2c, LM75, 2, I2C_MEMADD_SIZE_8BIT, hyst, 2);
    printf("L dmatx %d %d\n", st, wait_done());
    st = HAL_I2C_Mem_Read_DMA(&hi2c, LM75, 2, I2C_MEMADD_SIZE_8BIT, b, 4);
    printf("L dmarx %d %d %04x %04x\n", st, wait_done(), be16(b), be16(b + 2));
    st = HAL_I2C_Mem_Read_DMA(&hi2c, LM75, 0, I2C_MEMADD_SIZE_8BIT, b, 2);
    printf("L dmatemp %d %d %04x\n", st, wait_done(), be16(b));
    printf("L done\n");

    while (1) {
        HAL_Delay(100);
        HAL_I2C_Mem_Read(&hi2c, LM75, 0, I2C_MEMADD_SIZE_8BIT, b, 2, 10);
        printf("L t %04x\n", be16(b));
    }
}
