/* Light oscilloscope: triggered captures, a rolling stream, or pulse measurement of the merged light level. */
#ifndef SCOPE_H
#define SCOPE_H

#include <stdbool.h>
#include <stdint.h>

#define SCOPE_NMAX    4000U      /* largest frame (samples); the memory is shared with the pulse windows */
#define SCOPE_NMIN    16U
#define SCOPE_DECMAX  4096U      /* a sample is the mean of 1 .. 4096 scans (2.5 us .. 10.24 ms) */
#define SCOPE_POSTMAX 4000000U   /* longest trigger delay in samples */

/* pulse mode memory layout: rising-edge window, falling-edge window, min/max envelope of the whole pulse */
#define PULSE_ENV   256U                                   /* envelope buckets (2 samples each) */
#define PULSE_WIN   ((SCOPE_NMAX - 2 * PULSE_ENV) / 2)     /* samples per edge window */
#define PULSE_PRE   (PULSE_WIN / 4)                        /* rising window: samples before the trigger */
#define PULSE_TAIL  (PULSE_WIN / 4)                        /* falling window: samples after the end */
#define PULSE_END_N 40U                                    /* net samples below the level that end a pulse (100 us) */
#define PULSE_SMOOTH 2U                                    /* trigger / end decide on y += (x - y) / 4 (10 us) */

enum { SCOPE_AUTO, SCOPE_NORMAL, SCOPE_SINGLE, SCOPE_FREE };   /* FREE: no trigger, a new frame as soon as possible */

typedef struct {
  uint16_t dec;        /* a sample is the mean of `dec` scans: sample rate 400 kS/s / dec */
  uint16_t n;          /* samples per frame */
  uint32_t post;       /* samples captured after the trigger sample (may exceed n: trigger delay) */
  int32_t level;       /* trigger level in combined counts (COUNTS_PER_LUX per lux) */
  bool falling;        /* trigger edge (pulse mode: falling = dark pulse) */
  uint8_t mode;        /* SCOPE_AUTO / NORMAL / SINGLE */
  bool roll;           /* stream every sample instead of triggered frames */
  bool pulse;          /* pulse measurement: full rate, edge windows + envelope, any length */
  uint32_t maxp_ms;    /* pulse mode: longer "pulses" are dropped as a change of the ambient light */
  uint8_t tfk;         /* trigger filter: the level is compared with an average y += (x - y) / 2^tfk (0 = off) */
} scope_cfg_t;

typedef struct {       /* a finished pulse, valid until scope_frame_done() */
  const int16_t *rise, *fall, *env;
  uint32_t fall_n;     /* valid samples in the falling window */
  uint32_t fall_start; /* index of its first sample, counted from the trigger sample (= 0) */
  uint32_t end;        /* first sample of the closing run below the level, from the trigger */
  uint32_t count;      /* samples from the trigger to the end of the closing run (sum = mean * count) */
  int32_t mean;        /* mean counts over those samples */
  int32_t plateau;     /* brightest 4-sample (10 us) block mean (counts) */
  uint32_t env_n, env_bucket;
  bool clipped, timeout;
} scope_pulse_t;

void scope_config(const scope_cfg_t *c);
void scope_run(bool run);                  /* stop freezes the display; run (re)arms */
void scope_get(scope_cfg_t *c, bool *running);
void scope_process(const uint16_t *scans, uint32_t n);   /* DMA interrupt */

/* main loop */
void scope_poll(uint32_t now_ms);          /* auto-trigger timeout */
bool scope_frame(const int16_t **buf, uint32_t *start, uint32_t *n, bool *forced);   /* normal frame (ring) */
bool scope_pulse(scope_pulse_t *p);
void scope_frame_sent(uint32_t count);     /* main loop: this many samples (oldest first) are out: may be reused */
void scope_frame_done(uint32_t now_ms);    /* frame shipped: re-arm (unless single) */
uint32_t scope_roll_avail(void);
uint32_t scope_roll_read(int16_t *out, uint32_t max);

/* sample encoding: int16, exact below 16384 counts, 32-count steps above (up to ~530k counts); monotonic */
static inline int16_t scope_enc(int32_t c) {
  if (c < 16384) return (int16_t)(c < -16384 ? -16384 : c);
  c = 16384 + ((c - 16384) >> 5);
  return (int16_t)(c > 32767 ? 32767 : c);
}

#endif
