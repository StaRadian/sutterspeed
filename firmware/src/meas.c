/* Front-end calibration. Runs once per DMA block on a subset of the scans (every 8th), so it costs little:
   - last raw values, dark-offset learning ("zero"),
   - gain self-calibration on block means (see meas_block),
   - raw channel statistics on request ("adc"),
   - live level: mean of the block, peak-held until read. */
#include <string.h>
#include "meas.h"
#include "board.h"

#define SUB 8                                       /* use every 8th scan */

meas_cal_t meas_cal = {VBIAS_COUNTS, VBIAS_COUNTS, VBIAS_COUNTS, 11 * 256, 121 * 256};
static volatile uint16_t last_raw[3];
static volatile uint32_t zero_left;
static int64_t zero_sum[3];
static volatile int32_t level_max = INT32_MIN;
static int64_t g_num1, g_den1, g_num0, g_den0;      /* gain accumulators, main loop only */
static volatile int32_t gb_num1, gb_den1, gb_num0, gb_den0;   /* filled by the interrupt, taken by meas_task */
static bool learned1, learned0;
meas_stat_t meas_stat;
static volatile uint32_t stat_left;

NOINLINE static void zero_block(const uint16_t *s, uint32_t n) {
  uint32_t a0 = 0, a1 = 0, a2 = 0, k = n < zero_left ? n : zero_left;   /* 512 x 4095 fits 32 bits */
  for (uint32_t i = 0; i < k; i++, s += 3) { a0 += s[0]; a1 += s[1]; a2 += s[2]; }
  zero_sum[0] += a0; zero_sum[1] += a1; zero_sum[2] += a2;
  zero_left -= k;
  if (zero_left == 0) {
    meas_cal.o0 = (int32_t)(zero_sum[0] / 4096);
    meas_cal.o1 = (int32_t)(zero_sum[1] / 4096);
    meas_cal.o2 = (int32_t)(zero_sum[2] / 4096);
  }
}

NOINLINE static void stat_block(const uint16_t *s, uint32_t n) {
  uint32_t sum[3] = {0}, sq[3] = {0}, k = 0;      /* 64 x 4095^2 still fits 32 bits */
  meas_stat_t *const t = &meas_stat;
  for (uint32_t i = 0; i < n && k < stat_left; i += SUB, s += 3 * SUB, k++) {
    for (int c = 0; c < 3; c++) {
      const uint32_t v = s[c];
      sum[c] += v; sq[c] += v * v;
      if (v < t->min[c]) t->min[c] = (uint16_t)v;
      if (v > t->max[c]) t->max[c] = (uint16_t)v;
      if (v <= MEAS_RAIL || v >= MEAS_SAT) t->rail[c]++;
    }
  }
  for (int c = 0; c < 3; c++) { t->sum[c] += sum[c]; t->sq[c] += sq[c]; }
  t->n += k;
  stat_left -= k;
}

RAMFUNC void meas_block(const uint16_t *s, uint32_t n) {
  last_raw[0] = s[0]; last_raw[1] = s[1]; last_raw[2] = s[2];
  if (zero_left) zero_block(s, n);
  if (stat_left) stat_block(s, n);
  const meas_cal_t c = meas_cal;
  int32_t sum = 0, cnt = 0, a0 = 0, a1 = 0, a2 = 0;
  uint32_t lo1 = 4095, hi1 = 0, lo2 = 4095, hi2 = 0;
  for (uint32_t i = 0; i < n; i += SUB, s += 3 * SUB) {
    sum += meas_merge(s, &c);
    cnt++;
    const uint32_t v1 = s[1], v2 = s[2];
    a0 += s[0]; a1 += (int32_t)v1; a2 += (int32_t)v2;
    if (v1 < lo1) lo1 = v1;
    if (v1 > hi1) hi1 = v1;
    if (v2 < lo2) lo2 = v2;
    if (v2 > hi2) hi2 = v2;
  }
  /* Gain learning on block means. Single scans cannot be compared: the stages carry wide-band noise that is not
     shared between them (the channels are sampled 0.83 us apart). A pair counts when the more sensitive stage
     stays off both rails over the whole block (a clipped tail would bias its mean) and both are clearly above
     their offsets. */
  if (cnt) {
    const int32_t d0 = a0 - cnt * c.o0, d1 = a1 - cnt * c.o1, d2 = a2 - cnt * c.o2;
    if (lo2 > MEAS_RAIL && hi2 < MEAS_SAT && d2 > 400 * cnt && d1 > 30 * cnt) { gb_num1 += d2; gb_den1 += d1; }
    if (lo1 > MEAS_RAIL && hi1 < MEAS_SAT && d1 > 400 * cnt && d0 > 20 * cnt) { gb_num0 += d1; gb_den0 += d0; }
  }
  if (cnt) {
    int32_t lv = sum / cnt;
    if (lv > level_max) level_max = lv;
  }
}

void meas_task(void) {
  __disable_irq();
  int32_t n1 = gb_num1, e1 = gb_den1, n0 = gb_num0, e0 = gb_den0;
  gb_num1 = gb_den1 = gb_num0 = gb_den0 = 0;
  __enable_irq();
  g_num1 += n1; g_den1 += e1; g_num0 += n0; g_den0 += e0;
  if (g_den1 > 200000) {
    int32_t k = (int32_t)(g_num1 * 256 / g_den1);
    if (k > 9 * 256 && k < 13 * 256) { meas_cal.k1 = k; learned1 = true; }
  }
  if (g_den0 > 100000) {
    int32_t r = (int32_t)(g_num0 * 256 / g_den0);  /* stage 2 / stage 1 in Q8 */
    if (r > 9 * 256 && r < 13 * 256) { meas_cal.k0 = (int32_t)(((int64_t)meas_cal.k1 * r) >> 8); learned0 = true; }
  }
  if (g_den1 > (1LL << 40) || g_den0 > (1LL << 40)) { g_num1 >>= 1; g_den1 >>= 1; g_num0 >>= 1; g_den0 >>= 1; }
}

float meas_level_take(void) {
  __disable_irq();
  int32_t v = level_max;
  level_max = INT32_MIN;
  __enable_irq();
  return v == INT32_MIN ? 0.0f : (float)v / COUNTS_PER_LUX;
}

void meas_raw(uint16_t raw[3]) { raw[0] = last_raw[0]; raw[1] = last_raw[1]; raw[2] = last_raw[2]; }
void meas_zero(void) {
  memset(zero_sum, 0, sizeof zero_sum);
  g_num1 = g_den1 = g_num0 = g_den0 = 0;           /* ratios learned against the old offsets start over */
  zero_left = 4096;
}
bool meas_zero_busy(void) { return zero_left != 0; }
void meas_gains(float g[2], bool learned[2]) {
  g[0] = (float)meas_cal.k0 / (float)meas_cal.k1;  /* x11 / x1 */
  g[1] = (float)meas_cal.k1 / 256.0f;              /* x121 / x11 */
  learned[0] = learned0; learned[1] = learned1;
}

void meas_stat_start(uint32_t scans) {
  __disable_irq();
  memset(&meas_stat, 0, sizeof meas_stat);
  for (int c = 0; c < 3; c++) meas_stat.min[c] = 0xFFFF;
  stat_left = scans / SUB;
  __enable_irq();
}

bool meas_stat_busy(void) { return stat_left != 0; }
