/* Sutterspeed - camera shutter speed tester firmware (STM32C071G8, TinyUSB CDC). */
#include "board.h"
#include "cli.h"
#include "layout.h"
#include "stm32c0xx_ll_bus.h"
#include "stm32c0xx_ll_system.h"
#include "tusb.h"

void clock_init(void);
void acq_init(void);

/* ---------------------------------------------------------------- time base */
static volatile uint32_t ms_ticks;
void SysTick_Handler(void) { ms_ticks++; }
uint32_t millis(void) { return ms_ticks; }
uint32_t tusb_time_millis_api(void) { return ms_ticks; }

/* ---------------------------------------------------------------- reboot into a USB bootloader (boot/boot.c):
   ours (DFU, 1209:0002) or, with `rom`, the ST system bootloader (DfuSe, 0483:DF11) */
void request_bootloader(bool rom) {
  BOOT_REQUEST = rom ? BOOT_REQ_ROM : BOOT_REQ_DFU;
  NVIC_SystemReset();
}

/* a crash restarts into our bootloader: a broken firmware can still be replaced over USB */
void HardFault_Handler(void) { request_bootloader(false); }

/* ---------------------------------------------------------------- USB */
void USB_DRD_FS_IRQHandler(void) { tud_int_handler(0); }

int main(void) {
  clock_init();
  SysTick_Config(SYSCLK_HZ / 1000);
  NVIC_SetPriority(SysTick_IRQn, 2);

  LL_APB1_GRP1_EnableClock(LL_APB1_GRP1_PERIPH_USB);   /* PA11/PA12 are the USB pads (no remap) */
  NVIC_SetPriority(USB_DRD_FS_IRQn, 1);
  const tusb_rhport_init_t rh = {.role = TUSB_ROLE_DEVICE, .speed = TUSB_SPEED_FULL};
  tusb_init(0, &rh);

  acq_init();

  while (1) {
    tud_task();
    cli_task();
  }
}
