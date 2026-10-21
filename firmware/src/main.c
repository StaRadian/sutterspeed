/* Sutterspeed - camera shutter speed tester firmware (STM32C071G8, TinyUSB CDC). */
#include "board.h"
#include "cli.h"
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

/* ---------------------------------------------------------------- reboot into the ST system bootloader (USB DFU) */
#define BOOT_MAGIC 0xB007DF00UL
#define SYSMEM_BASE 0x1FFF0000UL
__attribute__((section(".noinit"))) static volatile uint32_t boot_request;

void request_bootloader(void) {
  boot_request = BOOT_MAGIC;
  NVIC_SystemReset();
}

static void jump_to_bootloader(void) {            /* called first thing after reset: peripherals untouched */
  LL_APB2_GRP1_EnableClock(LL_APB2_GRP1_PERIPH_SYSCFG);
  LL_SYSCFG_SetRemapMemory(LL_SYSCFG_REMAP_SYSTEMFLASH);
  uint32_t sp = *(volatile uint32_t *)SYSMEM_BASE;
  uint32_t pc = *(volatile uint32_t *)(SYSMEM_BASE + 4);
  __set_MSP(sp);
  ((void (*)(void))pc)();
  while (1) {}
}

/* ---------------------------------------------------------------- USB */
void USB_DRD_FS_IRQHandler(void) { tud_int_handler(0); }

int main(void) {
  if (boot_request == BOOT_MAGIC) {
    boot_request = 0;
    jump_to_bootloader();
  }

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
