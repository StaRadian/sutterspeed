/* Front-end calibration: dark offsets of the three gain stages, their gain ratios, and the rule that merges
   one ADC scan (stage 1 x1, stage 2 x11, stage 3 x121) into one wide-range value in stage-3 LSBs
   (COUNTS_PER_LUX per lux). Measuring and triggering is done by the scope (scope.c). */
#ifndef MEAS_H
#define MEAS_H

#include <stdbool.h>
#include <stdint.h>

#define MEAS_SAT 3900     /* raw ADC value treated as saturated */
#define MEAS_RAIL 16      /* raw ADC value at or below: the stage sits on its lower rail */

typedef struct {
  int32_t o0, o1, o2;     /* dark offsets (raw counts) */
  int32_t k1, k0;         /* Q8 gain of stage 2 / stage 1 relative to stage 3 */
} meas_cal_t;

extern meas_cal_t meas_cal;

/* merged value of one scan */
static inline int32_t meas_merge(const uint16_t *s, const meas_cal_t *c) {
  return s[2] < MEAS_SAT ? (int32_t)s[2] - c->o2
       : s[1] < MEAS_SAT ? (((int32_t)s[1] - c->o1) * c->k1) >> 8 : (((int32_t)s[0] - c->o0) * c->k0) >> 8;
}

void meas_block(const uint16_t *scans, uint32_t n);   /* DMA interrupt: raw values, zeroing, gain learning, level */
float meas_level_take(void);   /* live level in lux: brightest 1.28 ms block mean since the last call */
void meas_raw(uint16_t raw[3]);
void meas_zero(void);          /* cover the sensor, then call: learns dark offsets */
bool meas_zero_busy(void);
void meas_gains(float g[2], bool learned[2]);   /* ratios x11/x1 and x121/x11, and whether each was learned */

/* diagnostic statistics of the raw channels over a number of scans (every 8th is used) */
typedef struct {
  uint32_t n;                  /* samples used */
  uint64_t sum[3], sq[3];
  uint16_t min[3], max[3];
  uint32_t rail[3];            /* samples at a rail (<= MEAS_RAIL or >= MEAS_SAT) */
} meas_stat_t;
extern meas_stat_t meas_stat;
void meas_stat_start(uint32_t scans);
bool meas_stat_busy(void);
void meas_task(void);          /* main loop: folds the learned gains in */

#endif
