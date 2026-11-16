/**
 * Emulator test program: a USB CDC (virtual COM port) device on USB OTG FS
 * (CN13) that echoes what it receives in upper case. ST's USB device
 * library with the HAL's PCD driver (FIFOs, no DMA), glue after ST's
 * templates. The core runs at 168 MHz for the 48 MHz USB clock.
 * On the LCD: "ready"; a "?" from the host shows the bytes received so far.
 */
#include <stdio.h>
#include "LCD_GUI.h"
#include "init.h"
#include "lcd.h"
#include "stm32f4xx_hal.h"
#include "usbd_cdc.h"
#include "usbd_core.h"
#include "usbd_ctlreq.h"

static PCD_HandleTypeDef hpcd;
static USBD_HandleTypeDef usbd;

/* ---- 168 MHz, 48 MHz on the PLL's Q output ------------------------------ */

static void clock_168_mhz(void) {
    RCC_ClkInitTypeDef clk = {0};
    RCC_OscInitTypeDef osc = {0};
    /* off the PLL while it is reconfigured */
    clk.ClockType = RCC_CLOCKTYPE_SYSCLK;
    clk.SYSCLKSource = RCC_SYSCLKSOURCE_HSE;
    clk.AHBCLKDivider = RCC_SYSCLK_DIV1;
    clk.APB1CLKDivider = RCC_HCLK_DIV4;
    clk.APB2CLKDivider = RCC_HCLK_DIV2;
    if (HAL_RCC_ClockConfig(&clk, FLASH_LATENCY_5) != HAL_OK) {
        while (1) {}
    }
    osc.OscillatorType = RCC_OSCILLATORTYPE_HSE;
    osc.HSEState = RCC_HSE_ON;
    osc.PLL.PLLState = RCC_PLL_ON;
    osc.PLL.PLLSource = RCC_PLLSOURCE_HSE;
    osc.PLL.PLLM = 8;
    osc.PLL.PLLN = 336;
    osc.PLL.PLLP = RCC_PLLP_DIV2;
    osc.PLL.PLLQ = 7;
    if (HAL_RCC_OscConfig(&osc) != HAL_OK) {
        while (1) {}
    }
    clk.ClockType = RCC_CLOCKTYPE_SYSCLK | RCC_CLOCKTYPE_HCLK | RCC_CLOCKTYPE_PCLK1 | RCC_CLOCKTYPE_PCLK2;
    clk.SYSCLKSource = RCC_SYSCLKSOURCE_PLLCLK;
    if (HAL_RCC_ClockConfig(&clk, FLASH_LATENCY_5) != HAL_OK) {
        while (1) {}
    }
}

/* ---- PCD: pins, clock, interrupt ---------------------------------------- */

void HAL_PCD_MspInit(PCD_HandleTypeDef *h) {
    (void)h;
    GPIO_InitTypeDef pins = {0};
    __HAL_RCC_GPIOA_CLK_ENABLE();
    pins.Pin = GPIO_PIN_11 | GPIO_PIN_12;
    pins.Mode = GPIO_MODE_AF_PP;
    pins.Pull = GPIO_NOPULL;
    pins.Speed = GPIO_SPEED_FREQ_VERY_HIGH;
    pins.Alternate = GPIO_AF10_OTG_FS;
    HAL_GPIO_Init(GPIOA, &pins);
    __HAL_RCC_USB_OTG_FS_CLK_ENABLE();
    HAL_NVIC_SetPriority(OTG_FS_IRQn, 5, 0);
    HAL_NVIC_EnableIRQ(OTG_FS_IRQn);
}

void OTG_FS_IRQHandler(void) { HAL_PCD_IRQHandler(&hpcd); }

/* ---- PCD callbacks -> USB device library -------------------------------- */

void HAL_PCD_SetupStageCallback(PCD_HandleTypeDef *h) { USBD_LL_SetupStage(h->pData, (uint8_t *)h->Setup); }
void HAL_PCD_DataOutStageCallback(PCD_HandleTypeDef *h, uint8_t ep) {
    USBD_LL_DataOutStage(h->pData, ep, h->OUT_ep[ep].xfer_buff);
}
void HAL_PCD_DataInStageCallback(PCD_HandleTypeDef *h, uint8_t ep) {
    USBD_LL_DataInStage(h->pData, ep, h->IN_ep[ep].xfer_buff);
}
void HAL_PCD_SOFCallback(PCD_HandleTypeDef *h) { USBD_LL_SOF(h->pData); }
void HAL_PCD_ResetCallback(PCD_HandleTypeDef *h) {
    USBD_LL_SetSpeed(h->pData, USBD_SPEED_FULL);
    USBD_LL_Reset(h->pData);
}
void HAL_PCD_SuspendCallback(PCD_HandleTypeDef *h) { USBD_LL_Suspend(h->pData); }
void HAL_PCD_ResumeCallback(PCD_HandleTypeDef *h) { USBD_LL_Resume(h->pData); }
void HAL_PCD_ISOOUTIncompleteCallback(PCD_HandleTypeDef *h, uint8_t ep) { USBD_LL_IsoOUTIncomplete(h->pData, ep); }
void HAL_PCD_ISOINIncompleteCallback(PCD_HandleTypeDef *h, uint8_t ep) { USBD_LL_IsoINIncomplete(h->pData, ep); }
void HAL_PCD_ConnectCallback(PCD_HandleTypeDef *h) { USBD_LL_DevConnected(h->pData); }
void HAL_PCD_DisconnectCallback(PCD_HandleTypeDef *h) { USBD_LL_DevDisconnected(h->pData); }

/* ---- USB device library -> PCD ------------------------------------------ */

static USBD_StatusTypeDef status(HAL_StatusTypeDef s) { return s == HAL_OK ? USBD_OK : USBD_FAIL; }

USBD_StatusTypeDef USBD_LL_Init(USBD_HandleTypeDef *pdev) {
    hpcd.Instance = USB_OTG_FS;
    hpcd.Init.dev_endpoints = 4;
    hpcd.Init.speed = PCD_SPEED_FULL;
    hpcd.Init.dma_enable = 0;
    hpcd.Init.phy_itface = PCD_PHY_EMBEDDED;
    hpcd.Init.Sof_enable = 0;
    hpcd.Init.low_power_enable = 0;
    hpcd.Init.lpm_enable = 0;
    hpcd.Init.vbus_sensing_enable = 0;
    hpcd.Init.use_dedicated_ep1 = 0;
    hpcd.pData = pdev;
    pdev->pData = &hpcd;
    if (HAL_PCD_Init(&hpcd) != HAL_OK) {
        return USBD_FAIL;
    }
    HAL_PCDEx_SetRxFiFo(&hpcd, 0x80);
    HAL_PCDEx_SetTxFiFo(&hpcd, 0, 0x40);
    HAL_PCDEx_SetTxFiFo(&hpcd, 1, 0x80);
    HAL_PCDEx_SetTxFiFo(&hpcd, 2, 0x10);
    return USBD_OK;
}

USBD_StatusTypeDef USBD_LL_DeInit(USBD_HandleTypeDef *pdev) { return status(HAL_PCD_DeInit(pdev->pData)); }
USBD_StatusTypeDef USBD_LL_Start(USBD_HandleTypeDef *pdev) { return status(HAL_PCD_Start(pdev->pData)); }
USBD_StatusTypeDef USBD_LL_Stop(USBD_HandleTypeDef *pdev) { return status(HAL_PCD_Stop(pdev->pData)); }
USBD_StatusTypeDef USBD_LL_OpenEP(USBD_HandleTypeDef *pdev, uint8_t ep, uint8_t type, uint16_t mps) {
    return status(HAL_PCD_EP_Open(pdev->pData, ep, mps, type));
}
USBD_StatusTypeDef USBD_LL_CloseEP(USBD_HandleTypeDef *pdev, uint8_t ep) {
    return status(HAL_PCD_EP_Close(pdev->pData, ep));
}
USBD_StatusTypeDef USBD_LL_FlushEP(USBD_HandleTypeDef *pdev, uint8_t ep) {
    return status(HAL_PCD_EP_Flush(pdev->pData, ep));
}
USBD_StatusTypeDef USBD_LL_StallEP(USBD_HandleTypeDef *pdev, uint8_t ep) {
    return status(HAL_PCD_EP_SetStall(pdev->pData, ep));
}
USBD_StatusTypeDef USBD_LL_ClearStallEP(USBD_HandleTypeDef *pdev, uint8_t ep) {
    return status(HAL_PCD_EP_ClrStall(pdev->pData, ep));
}
uint8_t USBD_LL_IsStallEP(USBD_HandleTypeDef *pdev, uint8_t ep) {
    PCD_HandleTypeDef *h = pdev->pData;
    return (ep & 0x80U) ? h->IN_ep[ep & 0x7FU].is_stall : h->OUT_ep[ep & 0x7FU].is_stall;
}
USBD_StatusTypeDef USBD_LL_SetUSBAddress(USBD_HandleTypeDef *pdev, uint8_t addr) {
    return status(HAL_PCD_SetAddress(pdev->pData, addr));
}
USBD_StatusTypeDef USBD_LL_Transmit(USBD_HandleTypeDef *pdev, uint8_t ep, uint8_t *buf, uint32_t size) {
    return status(HAL_PCD_EP_Transmit(pdev->pData, ep, buf, size));
}
USBD_StatusTypeDef USBD_LL_PrepareReceive(USBD_HandleTypeDef *pdev, uint8_t ep, uint8_t *buf, uint32_t size) {
    return status(HAL_PCD_EP_Receive(pdev->pData, ep, buf, size));
}
uint32_t USBD_LL_GetRxDataSize(USBD_HandleTypeDef *pdev, uint8_t ep) { return HAL_PCD_EP_GetRxCount(pdev->pData, ep); }
void USBD_LL_Delay(uint32_t ms) { HAL_Delay(ms); }

/* the CDC class allocates its one handle */
static uint32_t class_mem[(sizeof(USBD_CDC_HandleTypeDef) + 3U) / 4U];
void *USBD_static_malloc(uint32_t size) {
    (void)size;
    return class_mem;
}
void USBD_static_free(void *p) { (void)p; }

/* ---- descriptors -------------------------------------------------------- */

static uint8_t device_desc[USB_LEN_DEV_DESC] = {
    0x12, USB_DESC_TYPE_DEVICE, 0x00, 0x02, 0x02, 0x02, 0x00, USB_MAX_EP0_SIZE,
    0x83, 0x04, 0x40, 0x57, /* 0483:5740, ST's virtual COM port */
    0x00, 0x02, USBD_IDX_MFC_STR, USBD_IDX_PRODUCT_STR, USBD_IDX_SERIAL_STR, USBD_MAX_NUM_CONFIGURATION};
static uint8_t langid_desc[USB_LEN_LANGID_STR_DESC] = {USB_LEN_LANGID_STR_DESC, USB_DESC_TYPE_STRING, 0x09, 0x04};
static uint8_t string_buf[USBD_MAX_STR_DESC_SIZ];

static uint8_t *string(const char *s, uint16_t *len) {
    USBD_GetString((uint8_t *)s, string_buf, len);
    return string_buf;
}
static uint8_t *get_device(USBD_SpeedTypeDef speed, uint16_t *len) {
    (void)speed;
    *len = sizeof(device_desc);
    return device_desc;
}
static uint8_t *get_langid(USBD_SpeedTypeDef speed, uint16_t *len) {
    (void)speed;
    *len = sizeof(langid_desc);
    return langid_desc;
}
static uint8_t *get_manufacturer(USBD_SpeedTypeDef speed, uint16_t *len) {
    (void)speed;
    return string("ITS-EMU", len);
}
static uint8_t *get_product(USBD_SpeedTypeDef speed, uint16_t *len) {
    (void)speed;
    return string("UsbCdcEcho", len);
}
static uint8_t *get_serial(USBD_SpeedTypeDef speed, uint16_t *len) {
    (void)speed;
    return string("0001", len);
}
static uint8_t *get_config(USBD_SpeedTypeDef speed, uint16_t *len) {
    (void)speed;
    return string("CDC", len);
}
static uint8_t *get_interface(USBD_SpeedTypeDef speed, uint16_t *len) {
    (void)speed;
    return string("CDC echo", len);
}
static USBD_DescriptorsTypeDef descriptors = {get_device,  get_langid, get_manufacturer, get_product,
                                              get_serial,  get_config, get_interface};

/* ---- the CDC interface: echo in upper case ------------------------------ */

static uint8_t rx_buf[CDC_DATA_FS_MAX_PACKET_SIZE];
static uint8_t tx_buf[CDC_DATA_FS_MAX_PACKET_SIZE];
static volatile uint32_t rx_len;
static volatile int rx_full;
static uint8_t line_coding[7] = {0x00, 0xC2, 0x01, 0x00, 0, 0, 8}; /* 115200 8N1 */
static uint32_t total;

static int8_t cdc_init(void) {
    USBD_CDC_SetRxBuffer(&usbd, rx_buf);
    return USBD_OK;
}
static int8_t cdc_deinit(void) { return USBD_OK; }
static int8_t cdc_control(uint8_t cmd, uint8_t *buf, uint16_t len) {
    if (cmd == CDC_SET_LINE_CODING && len >= 7) {
        memcpy(line_coding, buf, 7);
    } else if (cmd == CDC_GET_LINE_CODING && len >= 7) {
        memcpy(buf, line_coding, 7);
    }
    return USBD_OK;
}
/* a packet arrived: the main loop answers and then takes the next one */
static int8_t cdc_receive(uint8_t *buf, uint32_t *len) {
    (void)buf;
    rx_len = *len;
    rx_full = 1;
    return USBD_OK;
}
static int8_t cdc_transmit_done(uint8_t *buf, uint32_t *len, uint8_t ep) {
    (void)buf;
    (void)len;
    (void)ep;
    return USBD_OK;
}
static USBD_CDC_ItfTypeDef cdc_fops = {cdc_init, cdc_deinit, cdc_control, cdc_receive, cdc_transmit_done};

int main(void) {
    initITSboard();
    GUI_init(DEFAULT_BRIGHTNESS);
    clock_168_mhz();
    lcdPrintlnS("UsbCdcEcho");
    if (USBD_Init(&usbd, &descriptors, DEVICE_FS) != USBD_OK || USBD_RegisterClass(&usbd, &USBD_CDC) != USBD_OK ||
        USBD_CDC_RegisterInterface(&usbd, &cdc_fops) != USBD_OK || USBD_Start(&usbd) != USBD_OK) {
        lcdPrintlnS("USB init failed");
        while (1) {}
    }
    lcdPrintlnS("ready");
    while (1) {
        if (!rx_full) {
            continue;
        }
        uint32_t n = rx_len;
        for (uint32_t i = 0; i < n; i++) {
            uint8_t c = rx_buf[i];
            tx_buf[i] = (c >= 'a' && c <= 'z') ? (uint8_t)(c - 'a' + 'A') : c;
        }
        total += n;
        USBD_CDC_HandleTypeDef *cdc = (USBD_CDC_HandleTypeDef *)usbd.pClassData;
        while (cdc->TxState != 0U) {}
        USBD_CDC_SetTxBuffer(&usbd, tx_buf, n);
        USBD_CDC_TransmitPacket(&usbd);
        /* tx_buf is in use until the packet is out */
        while (cdc->TxState != 0U) {}
        rx_full = 0;
        USBD_CDC_ReceivePacket(&usbd);
        if (n == 1U && rx_buf[0] == '?') {
            char text[32];
            snprintf(text, sizeof(text), "rx %lu", (unsigned long)total);
            lcdGotoXY(0, 2);
            lcdPrintS(text);
        }
    }
}
