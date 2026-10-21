/* Acquisition: TIM3 TRGO (400 kHz) triggers a 3-channel ADC scan, DMA1 ch1 streams it into a circular buffer.
   Every half buffer (512 scans = 1.28 ms) is handed to the scope engine from the DMA interrupt. */
#include "board.h"
#include "meas.h"
#include "scope.h"
#include "stm32c0xx_ll_adc.h"
#include "stm32c0xx_ll_bus.h"
#include "stm32c0xx_ll_dma.h"
#include "stm32c0xx_ll_dmamux.h"
#include "stm32c0xx_ll_gpio.h"
#include "stm32c0xx_ll_tim.h"

static uint16_t dma_buf[2 * DMA_HALF_SCANS * ADC_NCH];
volatile uint32_t acq_overruns;
volatile uint32_t acq_isr_us_max, acq_isr_us_last;   /* processing time per DMA interrupt (budget 1280 us) */
volatile uint32_t acq_isr_calls;

static void delay_cycles(uint32_t n) {
  while (n--) __NOP();
}

void acq_init(void) {
  LL_IOP_GRP1_EnableClock(LL_IOP_GRP1_PERIPH_GPIOA);
  LL_GPIO_SetPinMode(GPIOA, LL_GPIO_PIN_0, LL_GPIO_MODE_ANALOG);
  LL_GPIO_SetPinMode(GPIOA, LL_GPIO_PIN_1, LL_GPIO_MODE_ANALOG);
  LL_GPIO_SetPinMode(GPIOA, LL_GPIO_PIN_2, LL_GPIO_MODE_ANALOG);

  /* ---- ADC: 24 MHz synchronous clock, 7.5-cycle sampling -> 20 cycles/conversion, 2.5 us per 3-ch scan ---- */
  LL_APB2_GRP1_EnableClock(LL_APB2_GRP1_PERIPH_ADC);
  LL_ADC_SetClock(ADC1, LL_ADC_CLOCK_SYNC_PCLK_DIV2);
  LL_ADC_EnableInternalRegulator(ADC1);
  delay_cycles(SYSCLK_HZ / 1000000 * LL_ADC_DELAY_INTERNAL_REGUL_STAB_US);
  LL_ADC_StartCalibration(ADC1);
  while (LL_ADC_IsCalibrationOnGoing(ADC1)) {}
  delay_cycles(64);

  LL_ADC_SetResolution(ADC1, LL_ADC_RESOLUTION_12B);
  LL_ADC_REG_SetSequencerConfigurable(ADC1, LL_ADC_REG_SEQ_FIXED);
  LL_ADC_REG_SetSequencerScanDirection(ADC1, LL_ADC_REG_SEQ_SCAN_DIR_FORWARD);
  LL_ADC_ClearFlag_CCRDY(ADC1);
  LL_ADC_REG_SetSequencerChannels(ADC1, LL_ADC_CHANNEL_0 | LL_ADC_CHANNEL_1 | LL_ADC_CHANNEL_2);
  while (!LL_ADC_IsActiveFlag_CCRDY(ADC1)) {}
  LL_ADC_SetSamplingTimeCommonChannels(ADC1, LL_ADC_SAMPLINGTIME_COMMON_1, LL_ADC_SAMPLINGTIME_7CYCLES_5);
  LL_ADC_REG_SetTriggerSource(ADC1, LL_ADC_REG_TRIG_EXT_TIM3_TRGO);
  LL_ADC_REG_SetTriggerEdge(ADC1, LL_ADC_REG_TRIG_EXT_RISING);
  LL_ADC_REG_SetDMATransfer(ADC1, LL_ADC_REG_DMA_TRANSFER_UNLIMITED);
  LL_ADC_REG_SetOverrun(ADC1, LL_ADC_REG_OVR_DATA_OVERWRITTEN);

  /* ---- DMA1 channel 1, circular, half/full interrupts ---- */
  LL_AHB1_GRP1_EnableClock(LL_AHB1_GRP1_PERIPH_DMA1);
  LL_DMA_SetPeriphRequest(DMA1, LL_DMA_CHANNEL_1, LL_DMAMUX_REQ_ADC1);
  LL_DMA_ConfigTransfer(DMA1, LL_DMA_CHANNEL_1,
                        LL_DMA_DIRECTION_PERIPH_TO_MEMORY | LL_DMA_MODE_CIRCULAR | LL_DMA_PERIPH_NOINCREMENT |
                            LL_DMA_MEMORY_INCREMENT | LL_DMA_PDATAALIGN_HALFWORD | LL_DMA_MDATAALIGN_HALFWORD |
                            LL_DMA_PRIORITY_HIGH);
  LL_DMA_ConfigAddresses(DMA1, LL_DMA_CHANNEL_1, LL_ADC_DMA_GetRegAddr(ADC1, LL_ADC_DMA_REG_REGULAR_DATA),
                         (uint32_t)dma_buf, LL_DMA_DIRECTION_PERIPH_TO_MEMORY);
  LL_DMA_SetDataLength(DMA1, LL_DMA_CHANNEL_1, sizeof(dma_buf) / sizeof(dma_buf[0]));
  LL_DMA_EnableIT_HT(DMA1, LL_DMA_CHANNEL_1);
  LL_DMA_EnableIT_TC(DMA1, LL_DMA_CHANNEL_1);
  NVIC_SetPriority(DMA1_Channel1_IRQn, 3);            /* lowest: USB (1) and SysTick (2) preempt it */
  NVIC_EnableIRQ(DMA1_Channel1_IRQn);
  LL_DMA_EnableChannel(DMA1, LL_DMA_CHANNEL_1);

  LL_ADC_ClearFlag_ADRDY(ADC1);
  LL_ADC_Enable(ADC1);
  while (!LL_ADC_IsActiveFlag_ADRDY(ADC1)) {}
  LL_ADC_REG_StartConversion(ADC1);                   /* armed; conversions start on each TIM3 TRGO */

  /* ---- TIM3: 400 kHz update -> TRGO ---- */
  LL_APB1_GRP1_EnableClock(LL_APB1_GRP1_PERIPH_TIM3);
  LL_TIM_SetPrescaler(TIM3, 0);
  LL_TIM_SetAutoReload(TIM3, SCAN_PERIOD_TICKS - 1);
  LL_TIM_SetTriggerOutput(TIM3, LL_TIM_TRGO_UPDATE);
  LL_TIM_EnableCounter(TIM3);

  /* ---- TIM14: free-running 1 MHz counter, only for measuring the interrupt load ---- */
  LL_APB2_GRP1_EnableClock(LL_APB2_GRP1_PERIPH_TIM14);
  LL_TIM_SetPrescaler(TIM14, SYSCLK_HZ / 1000000 - 1);
  LL_TIM_SetAutoReload(TIM14, 0xFFFF);
  LL_TIM_GenerateEvent_UPDATE(TIM14);
  LL_TIM_EnableCounter(TIM14);
}

void DMA1_Channel1_IRQHandler(void) {
  uint16_t t0 = (uint16_t)TIM14->CNT;
  acq_isr_calls++;
  /* both halves waiting: processing fell a whole half behind and data may have been overwritten. (One block
     taking a little longer than 1.28 ms is harmless: the other half is still being filled and is caught up.) */
  if (LL_DMA_IsActiveFlag_HT1(DMA1) && LL_DMA_IsActiveFlag_TC1(DMA1)) acq_overruns++;
  if (LL_DMA_IsActiveFlag_HT1(DMA1)) {
    LL_DMA_ClearFlag_HT1(DMA1);
    scope_process(&dma_buf[0], DMA_HALF_SCANS);
  }
  if (LL_DMA_IsActiveFlag_TC1(DMA1)) {
    LL_DMA_ClearFlag_TC1(DMA1);
    scope_process(&dma_buf[DMA_HALF_SCANS * ADC_NCH], DMA_HALF_SCANS);
  }
  uint32_t us = (uint16_t)((uint16_t)TIM14->CNT - t0);
  acq_isr_us_last = us;
  if (us > acq_isr_us_max) acq_isr_us_max = us;
}
