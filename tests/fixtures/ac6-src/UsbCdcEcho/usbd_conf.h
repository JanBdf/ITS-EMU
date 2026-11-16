/**
 * Emulator test program UsbCdcEcho: configuration of ST's USB device
 * library (after its usbd_conf_template.h).
 */
#ifndef USBD_CONF_H
#define USBD_CONF_H

#include "stm32f4xx_hal.h"
#include <stdlib.h>
#include <string.h>

#define USBD_MAX_NUM_INTERFACES 1U
#define USBD_MAX_NUM_CONFIGURATION 1U
#define USBD_MAX_STR_DESC_SIZ 64U
#define USBD_SELF_POWERED 1U
#define USBD_DEBUG_LEVEL 0U
#define DEVICE_FS 0

void *USBD_static_malloc(uint32_t size);
void USBD_static_free(void *p);

#define USBD_malloc USBD_static_malloc
#define USBD_free USBD_static_free
#define USBD_memset memset
#define USBD_memcpy memcpy
#define USBD_Delay HAL_Delay

#define USBD_UsrLog(...) do {} while (0)
#define USBD_ErrLog(...) do {} while (0)
#define USBD_DbgLog(...) do {} while (0)

#endif
