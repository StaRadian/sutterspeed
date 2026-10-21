/* Host test of the scope's pulse mode: synthetic light pulses (125 us .. 2 s, with finite rise/fall times,
   noise and room-light flicker) are turned into ADC scans, fed through scope_process() in DMA-sized blocks, and
   the captured windows are evaluated the way the viewer does (50 % crossings, interpolated).
   Build (VS developer prompt, from firmware/test):
     cl /nologo /O2 /W3 /I stub /I ..\src test_pulse.c ..\src\scope.c ..\src\meas.c /Fe:test_pulse.exe */
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include "board.h"
#include "meas.h"
#include "scope.h"

static double dec(int16_t e) { return e < 16384 ? e : 16384.0 + (double)(e - 16384) * 32.0; }

static uint32_t rng = 1;
static double noise(void) {                       /* ~N(0,1) */
  double s = 0;
  for (int i = 0; i < 4; i++) { rng = rng * 1664525u + 1013904223u; s += (rng >> 8) / 16777216.0 - 0.5; }
  return s * 1.732;
}

/* light in lux at time t (s) for a pulse starting at t0 with width w */
static double light(double t, double t0, double w, double peak, double amb, double tr) {
  double up = 1.0 / (1.0 + exp(-(t - t0) / (tr / 4.4)));
  double dn = 1.0 / (1.0 + exp(-(t - t0 - w) / (tr / 4.4)));
  return amb * (1.0 + 0.1 * sin(2 * 3.14159265 * 120 * t)) + peak * (up - dn);
}

static double fe_noise;                          /* front-end noise, lux rms per scan */
static void scan(uint16_t *s, double lux) {
  const double g[3] = {1, 11, 121};
  lux += fe_noise * noise();
  for (int c = 0; c < 3; c++) {
    double code = VBIAS_COUNTS + lux * COUNTS_PER_LUX / 121.0 * g[c] + 2.0 * noise();
    s[c] = (uint16_t)(code > 4095 ? 4095 : code < 0 ? 0 : code);
  }
}

static double cross(const int16_t *a, int n, double lvl, int up) {   /* first rise / last fall */
  if (up) {
    for (int i = 1; i < n; i++) if (dec(a[i - 1]) < lvl && dec(a[i]) >= lvl) return i - 1 + (lvl - dec(a[i - 1])) / (dec(a[i]) - dec(a[i - 1]));
  } else {
    for (int i = n - 1; i >= 1; i--) if (dec(a[i - 1]) >= lvl && dec(a[i]) < lvl) return i - 1 + (dec(a[i - 1]) - lvl) / (dec(a[i - 1]) - dec(a[i]));
  }
  return -1;
}

static int run_lvl(double w, double peak, double amb, double tr, double lvl) {
  static uint16_t blk[DMA_HALF_SCANS * 3];
  scope_cfg_t c = {.dec = 1, .n = 2000, .post = 0, .level = (int32_t)(lvl * COUNTS_PER_LUX),
                   .mode = SCOPE_SINGLE, .pulse = true, .maxp_ms = 40000};
  scope_config(&c);
  double t = 0, t0 = 0.01;
  scope_pulse_t p;
  for (;;) {
    for (uint32_t i = 0; i < DMA_HALF_SCANS; i++, t += DT_US * 1e-6) scan(&blk[3 * i], light(t, t0, w, peak, amb, tr));
    scope_process(blk, DMA_HALF_SCANS);
    if (scope_pulse(&p)) break;
    if (t > t0 + w + 1.0) { printf("  w=%.6f s: NO PULSE\n", w); return 1; }
  }
  /* viewer-side evaluation */
  double base = 0;
  for (uint32_t i = 0; i < PULSE_PRE - 20; i++) base += dec(p.rise[i]);
  base /= PULSE_PRE - 20;
  double P = p.plateau - base, half = base + P / 2;
  double r = cross(p.rise, PULSE_WIN, half, 1), f = cross(p.fall, (int)p.fall_n, half, 0);
  double width = ((double)p.fall_start + f - (r - PULSE_PRE)) * DT_US;   /* us */
  double err = width / (w * 1e6) - 1;
  int bad = r < 0 || f < 0 || fabs(err) > (w < 1e-3 ? 0.02 : 0.002);
  printf("  w=%10.1f us  measured %10.1f us  err %+7.3f %%  plateau %.0f lx  env %u x %u  %s\n", w * 1e6, width,
         err * 100, P / COUNTS_PER_LUX, p.env_n, p.env_bucket, bad ? "FAIL" : "ok");
  scope_frame_done(0);
  return bad;
}

static int run(double w, double peak, double amb, double tr) { return run_lvl(w, peak, amb, tr, amb + peak * 0.25); }

int main(void) {
  int fails = 0;
  const double ws[] = {125e-6, 250e-6, 1e-3, 2e-3, 4e-3, 8e-3, 1.0 / 60, 1.0 / 30, 1.0 / 8, 0.5, 1.0, 2.0};
  printf("pulse widths, 2000 lx on 20 lx ambient, 20 us edges:\n");
  for (unsigned i = 0; i < sizeof ws / sizeof ws[0]; i++) fails += run(ws[i], 2000, 20, 20e-6);
  printf("dim pulse, 100 lx:\n");
  fails += run(4e-3, 100, 20, 20e-6);
  printf("noisy front end (30 lx rms per scan), 400 lx pulse on 100 lx, level 2 rms above the room light:\n");
  fe_noise = 30;
  for (unsigned i = 2; i < 9; i++) fails += run_lvl(ws[i], 400, 100, 20e-6, 160);
  fe_noise = 0;
  printf("very bright, 20 klx (only stage 1 in range):\n");
  fails += run(4e-3, 20000, 20, 20e-6);
  printf("beyond the range, 100 klx: must be flagged as clipped\n");
  {
    static uint16_t blk[DMA_HALF_SCANS * 3];
    scope_cfg_t c = {.n = 2000, .level = 3000, .mode = SCOPE_SINGLE, .pulse = true, .maxp_ms = 40000};
    scope_config(&c);
    scope_pulse_t p;
    double t = 0;
    int got = 0;
    while (t < 0.1 && !got) {
      for (uint32_t i = 0; i < DMA_HALF_SCANS; i++, t += DT_US * 1e-6) scan(&blk[3 * i], light(t, 0.01, 4e-3, 100000, 20, 20e-6));
      scope_process(blk, DMA_HALF_SCANS);
      got = scope_pulse(&p);
    }
    printf("  clipped flag %d  %s\n", got ? p.clipped : -1, got && p.clipped ? "ok" : "FAIL");
    fails += !(got && p.clipped);
    scope_frame_done(0);
  }
  printf("decimation by any integer (mean of a ramp):\n");
  for (int d = 1; d <= 4096; d = d < 8 ? d + 1 : d * 3) {
    static uint16_t blk[DMA_HALF_SCANS * 3];
    scope_cfg_t c = {.dec = (uint16_t)d, .n = 64, .post = 32, .level = 1, .mode = SCOPE_AUTO, .maxp_ms = 40000};
    scope_config(&c);
    const int16_t *b;
    uint32_t st0, n, scan = 0;
    bool forced;
    int got = 0;
    const uint32_t period = (uint32_t)d * (d < 3000 ? 3000u / (uint32_t)d : 1u);   /* groups never straddle a wrap */
    while (!got && scan < 400000u * 8) {
      for (uint32_t i = 0; i < DMA_HALF_SCANS; i++, scan++) {   /* stage 3 ramps, period a multiple of d */
        blk[3 * i] = blk[3 * i + 1] = VBIAS_COUNTS;
        blk[3 * i + 2] = (uint16_t)(VBIAS_COUNTS + scan % period);
      }
      scope_process(blk, DMA_HALF_SCANS);
      scope_poll(scan / 400);
      got = scope_frame(&b, &st0, &n, &forced);
    }
    /* consecutive samples are means of consecutive groups of d scans: the ramp steps by exactly d */
    int bad = !got;
    if (got && d < 1500) {
      for (uint32_t i = 1; i < n; i++) {
        int dv = b[(st0 + i) % n] - b[(st0 + i - 1) % n];
        if (dv < 0) continue;                     /* ramp wrap */
        if (abs(dv - d) > 1) bad = 1;
      }
    }
    printf("  dec %4d: %s\n", d, bad ? "FAIL" : "ok");
    fails += bad;
    scope_frame_done(0);
  }
  printf("%s (%d failures)\n", fails ? "FAILED" : "all ok", fails);
  return fails != 0;
}
