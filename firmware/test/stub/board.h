/* Host-test stand-in for src/board.h: same constants, no CMSIS. */
#ifndef BOARD_H
#define BOARD_H
#define SYSCLK_HZ 48000000UL
#define ADC_NCH 3
#define SCAN_PERIOD_TICKS 120U
#define DT_US (SCAN_PERIOD_TICKS * 1e6f / SYSCLK_HZ)
#define DMA_HALF_SCANS 512U
#define COUNTS_PER_LUX 15.02f
#define VBIAS_COUNTS 372
#define RAMFUNC
#define NOINLINE
#define PROF_NOW() 0
static inline void __disable_irq(void) {}
static inline void __enable_irq(void) {}
#endif
