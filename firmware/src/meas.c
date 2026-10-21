/* Front-end calibration. Runs once per DMA block on a subset of the scans (every 8th), so it costs little:
   - last raw values, dark-offset learning ("zero"),
   - gain self-calibration: whenever two neighbouring stages are both mid-range their ratio is accumulated,
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

NOINLINE static void zero_block(const uint16_t *s, uint32_t n) {
  for (uint32_t i = 0; i < n && zero_left; i++, s += 3) {
    zero_sum[0] += s[0]; zero_sum[1] += s[1]; zero_sum[2] += s[2];
    if (--zero_left == 0) {
      meas_cal.o0 = (int32_t)(zero_sum[0] / 4096);
      meas_cal.o1 = (int32_t)(zero_sum[1] / 4096);
      meas_cal.o2 = (int32_t)(zero_sum[2] / 4096);
    }
  }
}

RAMFUNC void meas_block(const uint16_t *s, uint32_t n) {
  last_raw[0] = s[0]; last_raw[1] = s[1]; last_raw[2] = s[2];
  if (zero_left) zero_block(s, n);
  const meas_cal_t c = meas_cal;
  int32_t sum = 0, cnt = 0, n1 = 0, e1 = 0, n0 = 0, e0 = 0;
  for (uint32_t i = 0; i < n; i += SUB, s += 3 * SUB) {
    sum += meas_merge(s, &c);
    cnt++;
    const int32_t v1 = s[1], d1 = v1 - c.o1;          /* both channels of a pair mid-range */
    if (d1 <= 40) continue;
    const int32_t v2 = s[2];
    if (v2 < 3500 && v2 - c.o2 > 400) { n1 += v2 - c.o2; e1 += d1; }
    if (v1 < 3500 && d1 > 400) { const int32_t d0 = (int32_t)s[0] - c.o0; if (d0 > 40) { n0 += d1; e0 += d0; } }
  }
  gb_num1 += n1; gb_den1 += e1; gb_num0 += n0; gb_den0 += e0;
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
  if (g_den1 > 200000) { int32_t k = (int32_t)(g_num1 * 256 / g_den1); if (k > 9 * 256 && k < 13 * 256) meas_cal.k1 = k; }
  if (g_den0 > 200000) {
    int32_t r = (int32_t)(g_num0 * 256 / g_den0);  /* stage 2 / stage 1 in Q8 */
    if (r > 9 * 256 && r < 13 * 256) meas_cal.k0 = (int32_t)(((int64_t)meas_cal.k1 * r) >> 8);
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
void meas_zero(void) { memset(zero_sum, 0, sizeof zero_sum); zero_left = 4096; }
bool meas_zero_busy(void) { return zero_left != 0; }
void meas_gains(float g[2]) { g[0] = (float)meas_cal.k0 / (float)meas_cal.k1; g[1] = (float)meas_cal.k0 / 256.0f; }
