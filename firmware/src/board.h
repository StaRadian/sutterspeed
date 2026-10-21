/* Sutterspeed board constants (schematic rev 2). */
#ifndef BOARD_H
#define BOARD_H

#include "stm32c0xx.h"

#define SYSCLK_HZ     48000000UL

/* ADC: PA0 = stage 1 TIA (x1), PA1 = stage 2 (x11), PA2 = stage 3 (x121), scanned in that order */
#define ADC_NCH       3
#define SCAN_PERIOD_TICKS 120U                        /* TIM3 @ 48 MHz -> 400 kHz scan rate */
#define DT_US         (SCAN_PERIOD_TICKS * 1e6f / SYSCLK_HZ)   /* 2.5 us per scan */
#define DMA_HALF_SCANS 512U                           /* 1.28 ms per DMA half */

/* analog front end (see doc): TEMD5510 ~10 nA/lx (illuminant A), Rf 10k, gains 1 / 11 / 121, ADC 3.3 V/4096
   combined value unit = stage-3 LSB  ->  10 nA * 10k * 121 / (3.3/4096) = 15.02 counts per lux */
#define COUNTS_PER_LUX 15.02f
#define VBIAS_COUNTS   372                            /* 0.3 V bias, default dark offset of every channel */

/* hot code runs from RAM: no flash wait states (1 WS at 48 MHz) */
#define RAMFUNC  __attribute__((section(".RamFunc"), noinline))
#define PROF_NOW() ((uint16_t)TIM14->CNT)              /* 1 MHz free-running (acq.c), for load measurements */
#define NOINLINE __attribute__((noinline))            /* rarely-run code: keep it in flash, out of the RAM hot path */

#endif
