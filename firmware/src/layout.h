/* Flash layout shared by the bootloader (boot/) and the firmware: the bootloader owns the first BOOT_SIZE bytes,
   the firmware's vector table starts right behind it. A word at the top of the RAM, outside both programs,
   carries a request from the firmware to the bootloader across a reset. */
#ifndef LAYOUT_H
#define LAYOUT_H

#include <stdint.h>

#define FLASH_START   0x08000000UL
#define FLASH_TOP     (FLASH_START + 64 * 1024)    /* STM32C071G8 */
#define FLASH_PAGE    2048U
#define BOOT_SIZE     0x3000UL                     /* 12 KB; keep in step with the linker scripts and dfu.rs */
#define APP_BASE      (FLASH_START + BOOT_SIZE)
#define RAM_START     0x20000000UL
#define RAM_TOP       (RAM_START + 24 * 1024)

#define BOOT_REQUEST  (*(volatile uint32_t *)(RAM_TOP - 16))   /* linker scripts leave the last 16 bytes out */
#define BOOT_REQ_DFU  0xB007DF00UL                 /* stay in the sutterspeed bootloader (USB DFU) */
#define BOOT_REQ_ROM  0xB007D0F0UL                 /* start the ST system bootloader (USB 0483:DF11) */
#define SYSMEM_BASE   0x1FFF0000UL

#endif
