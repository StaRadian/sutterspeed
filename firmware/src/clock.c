/* 48 MHz from HSI48 (no crystal on this board); USB clocked by HSIUSB48 trimmed by CRS from USB SOF. */
#include "board.h"
#include "stm32c0xx_ll_bus.h"
#include "stm32c0xx_ll_crs.h"
#include "stm32c0xx_ll_rcc.h"
#include "stm32c0xx_ll_system.h"

void clock_init(void) {
  LL_FLASH_SetLatency(LL_FLASH_LATENCY_1);
  while (LL_FLASH_GetLatency() != LL_FLASH_LATENCY_1) {}

  LL_RCC_SetHSIDiv(LL_RCC_HSI_DIV_1);                 /* HSISYS = 48 MHz (reset value is /4) */
  LL_RCC_SetSysClkSource(LL_RCC_SYS_CLKSOURCE_HSI);
  while (LL_RCC_GetSysClkSource() != LL_RCC_SYS_CLKSOURCE_STATUS_HSI) {}
  LL_RCC_SetAHBPrescaler(LL_RCC_SYSCLK_DIV_1);
  LL_RCC_SetAPB1Prescaler(LL_RCC_APB1_DIV_1);
  SystemCoreClock = SYSCLK_HZ;

  RCC->CR |= RCC_CR_HSIUSB48ON;                       /* 48 MHz USB oscillator */
  while (!(RCC->CR & RCC_CR_HSIUSB48RDY)) {}
  LL_RCC_SetUSBClockSource(LL_RCC_USB_CLKSOURCE_HSI48);

  LL_APB1_GRP1_EnableClock(LL_APB1_GRP1_PERIPH_CRS);  /* trim HSIUSB48 against the 1 kHz USB SOF */
  LL_CRS_SetSyncSignalSource(LL_CRS_SYNC_SOURCE_USB);
  LL_CRS_EnableAutoTrimming();
  LL_CRS_EnableFreqErrorCounter();
}
