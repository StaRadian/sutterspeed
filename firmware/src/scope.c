/* Light oscilloscope engine (DMA interrupt). Every scan is merged into one value (meas_merge), then:
   - normal: averaged over `dec` scans into a ring of n samples with a fill / armed /
     post-trigger state machine; the frame is the last n samples once `post` samples followed the trigger, so the
     trigger may sit anywhere in the frame or before it (trigger delay). The frame is frozen until it is sent.
   - roll: the decimated samples go through a small queue.
   - pulse: full rate. A rising-edge window around the trigger, a falling-edge window around the end of the
     pulse and a min/max envelope of the whole pulse (buckets double when full), so any length is captured
     with 2.5 us edges. Start and end are decided on a lightly smoothed value (the 2.5 us samples are noisy);
     the pulse ends once PULSE_END_N more samples were below the level than (weighted) above it, so noise around
     the level cannot hold it open. The windows keep the raw samples. */
#include "scope.h"
#include "board.h"
#include "meas.h"

#define ROLLQ 2048U                /* roll queue in the frame memory (unused while rolling); power of two */
#define FALL  (PULSE_WIN)          /* falling window = samples [PULSE_WIN, 2*PULSE_WIN) of mem */
#define ENV0  (2 * PULSE_WIN)      /* envelope pairs follow */

enum { S_STOP, S_FILL, S_ARMED, S_POST, S_DONE, S_HIGH, S_TAIL, S_SEND };
/* S_SEND: a frame is being sent. New samples already refill the history, but only over samples that are out. */

static int16_t mem[SCOPE_NMAX];   /* frame ring / pulse windows / roll queue */
static scope_cfg_t cfg = {.dec = 1, .n = 2000, .post = 1000, .level = 600, .mode = SCOPE_AUTO, .maxp_ms = 40000};
static volatile uint8_t st = S_STOP;
static bool running;
static uint32_t wi, filled, post_left;
static int32_t prev, acc;
static uint32_t k;
static uint32_t send_start, send_written;
static int32_t tf_y8;          /* trigger filter state, counts x 256 */
static bool tf_ok;
static volatile uint32_t send_sent;
static uint32_t dec_recip = 1u << 24;
static volatile bool force;
static bool forced;
static uint32_t armed_ms;
#define rollq mem
static volatile uint32_t rq_w, rq_r;

/* pulse state */
static uint32_t p_fw, p_fcount, p_cnt, p_end, p_low, p_tail, p_maxp;
static int64_t p_sum;
static int32_t p_q, p_p4;      /* 4-sample block: running sum, brightest block sum */
static uint32_t p_qn;
static uint32_t e_n, e_bucket, e_cnt;
static int32_t e_min, e_max;
static bool p_clip, p_timeout, p_rdone;
static uint32_t p_w0;           /* rising-window write position right after the trigger sample */
static int32_t p_y;             /* smoothed value for the start / end decisions */
static bool p_yok;

static void rearm(void) {          /* call with the DMA interrupt masked or from it */
  wi = 0; filled = 0; k = 0; acc = 0; force = false; p_yok = false;
  st = running ? S_FILL : S_STOP;
}

static inline uint32_t need_pre(void) {   /* history needed before the trigger may fire */
  if (cfg.pulse) return PULSE_PRE;
  return cfg.post >= cfg.n - 1u ? 0 : cfg.n - 1u - cfg.post;
}

void scope_config(const scope_cfg_t *c) {
  __disable_irq();
  /* only the trigger level / edge changed: swap them in place, keep the record and the frame being sent */
  if (running && !c->pulse && !cfg.pulse && c->dec == cfg.dec && c->n == cfg.n && c->post == cfg.post &&
      c->mode == cfg.mode && c->roll == cfg.roll && c->tfk == cfg.tfk && c->maxp_ms == cfg.maxp_ms &&
      (c->level != cfg.level || c->falling != cfg.falling)) {
    cfg.level = c->level;
    cfg.falling = c->falling;
    __enable_irq();
    return;
  }
  cfg = *c;
  if (cfg.dec < 1) cfg.dec = 1;
  if (cfg.dec > SCOPE_DECMAX) cfg.dec = SCOPE_DECMAX;
  dec_recip = (uint32_t)((1u << 24) / cfg.dec);   /* mean = sum * recip >> 24 (no divide on the M0+) */
  if (cfg.n > SCOPE_NMAX) cfg.n = SCOPE_NMAX;
  if (cfg.n < SCOPE_NMIN) cfg.n = SCOPE_NMIN;
  if (cfg.post > SCOPE_POSTMAX) cfg.post = SCOPE_POSTMAX;
  if (cfg.tfk > 15) cfg.tfk = 15;
  tf_ok = false;
  if (cfg.pulse) { cfg.dec = 1; dec_recip = 1u << 24; cfg.roll = false; if (cfg.mode == SCOPE_AUTO) cfg.mode = SCOPE_NORMAL; }
  p_maxp = (uint32_t)((float)cfg.maxp_ms * 1000.0f / DT_US);
  rq_w = rq_r = 0;
  running = true;
  rearm();
  __enable_irq();
}

void scope_run(bool run) {
  __disable_irq();
  running = run;
  if (run) { rq_w = rq_r = 0; rearm(); }
  else if (st != S_DONE) st = S_STOP;            /* a finished frame is still sent */
  __enable_irq();
}

void scope_get(scope_cfg_t *c, bool *run) { *c = cfg; *run = running; }

/* ---------------------------------------------------------------- normal / roll */

/* State machine over a chunk of decimated samples; state lives in locals (few registers on the M0+). */
RAMFUNC static void feed(const int32_t *v, uint32_t m) {
  if (cfg.roll) {
    if (!running) return;
    uint32_t w = rq_w;
    for (uint32_t j = 0; j < m && w - rq_r < ROLLQ; j++, w++) rollq[w & (ROLLQ - 1)] = scope_enc(v[j]);
    rq_w = w;
    return;
  }
  uint32_t state = st, w = wi, f = filled, pl = post_left;
  int32_t pv = prev;
  if (state == S_SEND) {                          /* refill the history behind the sender */
    int16_t *const bf = mem;
    const uint32_t n = cfg.n, sent = send_sent;
    uint32_t wr = send_written;
    const uint32_t tk = cfg.tfk;
    int32_t y8 = tf_y8;
    for (uint32_t j = 0; j < m; j++) {
      if (tk) y8 += (v[j] * 256 - y8) >> tk;
      if (wr < sent) {
        bf[w] = scope_enc(v[j]);
        if (++w == n) w = 0;
        wr++;
        f++;
      } else {
        f = 0;                                    /* overtook the sender: the history restarts */
      }
    }
    if (m) pv = tk ? y8 / 256 : v[m - 1];
    tf_y8 = y8;
    wi = w; filled = f; prev = pv; send_written = wr;
    return;
  }
  const int32_t lv = cfg.level;
  const uint32_t pre = need_pre(), post = cfg.post, n = cfg.n;
  const bool fall = cfg.falling;
  int16_t *const bf = mem;
  const uint32_t tk = cfg.tfk;
  int32_t y8 = tf_y8;
  if (tk && !tf_ok && m) { y8 = v[0] * 256; tf_ok = true; }
  for (uint32_t j = 0; j < m; j++) {
    int32_t x = v[j];
    bf[w] = scope_enc(x);
    if (tk) {                                     /* trigger on the smoothed value (the stored sample stays raw) */
      y8 += (x * 256 - y8) >> tk;
      x = y8 / 256;
    }
    if (state == S_ARMED) {
      const bool hit = cfg.mode == SCOPE_FREE || (fall ? (pv >= lv && x < lv) : (pv < lv && x >= lv));
      if (hit || force) {
        forced = !hit;
        force = false;
        pl = post;
        state = pl ? S_POST : S_DONE;
      }
    } else if (state == S_FILL) {
      if (++f >= pre) state = S_ARMED;
    } else if (--pl == 0) {                       /* S_POST */
      state = S_DONE;
    }
    if (++w == n) w = 0;
    pv = x;
    if (state == S_DONE) break;
  }
  wi = w; filled = f; post_left = pl; prev = pv; tf_y8 = y8;
  st = (uint8_t)state;
}

/* ---------------------------------------------------------------- pulse */

RAMFUNC static void env_push(void) {                /* e_min / e_max are 4-sample sums */
  mem[ENV0 + 2 * e_n] = scope_enc(e_min / 4);
  mem[ENV0 + 2 * e_n + 1] = scope_enc(e_max / 4);
  e_cnt = 0;
  e_min = INT32_MAX; e_max = INT32_MIN;
  if (++e_n == PULSE_ENV) {                       /* full: halve the resolution (the encoding keeps the order) */
    int16_t *e = &mem[ENV0];
    for (uint32_t j = 0; j < PULSE_ENV / 2; j++) {
      int16_t a0 = e[4 * j], a1 = e[4 * j + 1], b0 = e[4 * j + 2], b1 = e[4 * j + 3];
      e[2 * j] = a0 < b0 ? a0 : b0;
      e[2 * j + 1] = a1 > b1 ? a1 : b1;
    }
    e_n = PULSE_ENV / 2;
    e_bucket *= 2;
  }
}

/* rising window before / at the trigger: returns the index of the trigger sample in v[], or m */
RAMFUNC static uint32_t pulse_wait(const int32_t *v, uint32_t m) {
  uint32_t w = wi, f = filled, state = st;
  int32_t pv = prev;
  const int32_t lv = cfg.falling ? -cfg.level : cfg.level;
  const int32_t sg = cfg.falling ? -1 : 0;        /* (x ^ sg) - sg = -x for a falling edge, without a multiply */
  int32_t y = p_y;
  if (!p_yok && m) { y = v[0]; p_yok = true; }
  uint32_t j = 0;
  for (; j < m; j++) {
    const int32_t x = v[j];
    mem[w] = scope_enc(x);
    if (++w == PULSE_WIN) w = 0;
    y += (x - y) >> PULSE_SMOOTH;
    const int32_t xs = (y ^ sg) - sg;
    if (state == S_ARMED) {
      if (pv < lv && xs >= lv) break;
    } else if (++f >= PULSE_PRE) {
      state = S_ARMED;
    }
    pv = xs;
  }
  wi = w; filled = f; prev = pv; st = (uint8_t)state; p_y = y;
  return j;
}

/* trigger sample x: start a pulse */
RAMFUNC static void pulse_start(int32_t x) {
  p_w0 = wi;
  p_rdone = false;
  mem[FALL] = scope_enc(x);
  p_fw = 1; p_fcount = 1; p_cnt = 1;
  p_sum = x; p_q = x; p_qn = 1; p_p4 = INT32_MIN;
  p_low = 0; p_end = 0; p_clip = false; p_timeout = false;
  e_n = 0; e_bucket = 4; e_cnt = 0; e_min = INT32_MAX; e_max = INT32_MIN;   /* 16-sample (40 us) buckets to start */
  st = S_HIGH;
}

/* The samples after the trigger that complete the rising window are the first ones of the falling-window
   ring: copy them over once, before that ring wraps (or when the pulse is shipped, if it never did). */
#define RISE_POST (PULSE_WIN - PULSE_PRE - 1)
RAMFUNC static void rise_copy(void) {
  const int16_t *fb = &mem[FALL + 1];
  uint32_t w = p_w0;
  for (uint32_t k = 0; k < RISE_POST; k++) {
    mem[w] = fb[k];
    if (++w == PULSE_WIN) w = 0;
  }
  wi = w;
  p_rdone = true;
}

/* append samples to the falling-window ring */
RAMFUNC static void fall_write(const int32_t *v, uint32_t m) {
  int16_t *const fb = &mem[FALL];
  uint32_t fw = p_fw;
  for (uint32_t j = 0; j < m; j++) {
    fb[fw] = scope_enc(v[j]);
    if (++fw == PULSE_WIN) { fw = 0; if (!p_rdone) rise_copy(); }
  }
  p_fw = fw;
  p_fcount += m;
}

/* 4-sample means of the pulse body: plateau (brightest) and the min/max envelope */
RAMFUNC static void body_blocks(const int32_t *v, uint32_t m) {
  int32_t q = p_q, p4 = p_p4, mn = e_min, mx = e_max;
  uint32_t qn = p_qn, ec = e_cnt, eb = e_bucket;
  for (uint32_t j = 0; j < m; j++) {
    q += v[j];
    if (++qn < 4) continue;
    if (q > p4) p4 = q;
    if (q < mn) mn = q;
    if (q > mx) mx = q;
    q = 0; qn = 0;
    if (++ec >= eb) { e_min = mn; e_max = mx; env_push(); eb = e_bucket; mn = INT32_MAX; mx = INT32_MIN; ec = 0; }
  }
  p_q = q; p_qn = qn; p_p4 = p4; e_min = mn; e_max = mx; e_cnt = ec;
}

/* samples after the trigger; short passes, each with few live values */
RAMFUNC static void pulse_run(const int32_t *v, uint32_t m) {
  if (st == S_HIGH) {
    /* pass 1: sum and end detection (stops at the end) */
    const int32_t lv = cfg.falling ? -cfg.level : cfg.level;
    const int32_t sg = cfg.falling ? -1 : 0;     /* (x ^ sg) - sg = -x for a dark pulse, no multiply */
    const uint32_t maxp = p_maxp;
    int32_t sum = 0, y = p_y;
    uint32_t low = p_low, cnt = p_cnt, je = m;
    for (uint32_t j = 0; j < m; j++) {
      const int32_t x = v[j];
      sum += x;
      cnt++;
      y += (x - y) >> PULSE_SMOOTH;
      /* below: one step towards the end; above: eight back (noise must be mostly below to end it) */
      low = ((y ^ sg) - sg) < lv ? low + 1 : low > 8 ? low - 8 : 0;
      if (low >= PULSE_END_N || cnt > maxp) { je = j + 1; break; }
    }
    p_sum += sum; p_low = low; p_cnt = cnt; p_y = y;
    body_blocks(v, je);                           /* pass 2 */
    if (je < m || low >= PULSE_END_N || cnt > maxp) {
      p_timeout = low < PULSE_END_N;
      p_end = cnt - low;                          /* first sample of the closing run */
      if (e_cnt) { env_push(); }                  /* partial last envelope bucket */
      st = S_TAIL;
      uint32_t t = m - je < PULSE_TAIL ? m - je : PULSE_TAIL;
      fall_write(v, je + t);                      /* pass 3: body + the first tail samples */
      p_tail = PULSE_TAIL - t;
    } else {
      fall_write(v, m);
    }
  } else if (st == S_TAIL) {
    /* the tail; a short pulse also waits until the rising window can be completed from the ring */
    uint32_t need = p_fcount < RISE_POST + 1 ? RISE_POST + 1 - p_fcount : 0;
    if (need < p_tail) need = p_tail;
    uint32_t t = need < m ? need : m;
    fall_write(v, t);
    p_tail = p_tail > t ? p_tail - t : 0;
  }
  if (st == S_TAIL && p_tail == 0 && p_fcount >= RISE_POST + 1) st = S_DONE;
}

RAMFUNC static void pulse_feed(const int32_t *v, uint32_t m) {
  while (m) {
    if (st == S_FILL || st == S_ARMED) {
      uint32_t j = pulse_wait(v, m);
      if (j == m) return;
      pulse_start(v[j]);
      v += j + 1; m -= j + 1;
    } else if (st == S_HIGH || st == S_TAIL) {
      pulse_run(v, m);
      return;
    } else {
      return;
    }
  }
}

/* ---------------------------------------------------------------- interrupt entry */

RAMFUNC void scope_process(const uint16_t *s, uint32_t n) {
  meas_block(s, n);
  if (!cfg.roll && (st == S_STOP || st == S_DONE)) return;
  const meas_cal_t c = meas_cal;
  const uint32_t d = cfg.dec, recip = dec_recip;
  const bool pulse = cfg.pulse;
  int32_t v[64];
  while (n) {
    uint32_t m = 0;
    if (d == 1) {                                 /* stage 1: merge (and decimate) into v[] */
      m = n < 64 ? n : 64;
      for (uint32_t j = 0; j < m; j++, s += 3) {
        v[j] = meas_merge(s, &c);
        if (s[0] >= MEAS_SAT) p_clip = true;
      }
      n -= m;
    } else {
      int32_t a = acc;
      uint32_t kk = k;
      for (; n && m < 64; n--, s += 3) {
        int32_t x = meas_merge(s, &c);
        a += x;                                   /* 4096 x 530k still fits in 32 bits */
        if (++kk >= d) { v[m++] = (int32_t)(((int64_t)a * recip + (1 << 23)) >> 24); a = 0; kk = 0; }
      }
      acc = a; k = kk;
    }
    if (m) {                                      /* stage 2: trigger state machine */
      if (pulse) pulse_feed(v, m);
      else feed(v, m);
    }
    if (!cfg.roll && (st == S_DONE || st == S_STOP)) return;
  }
}

/* ---------------------------------------------------------------- main loop side */

void scope_poll(uint32_t now_ms) {
  if (cfg.roll || cfg.pulse || cfg.mode != SCOPE_AUTO) return;
  if (st == S_FILL || st == S_POST || st == S_SEND) { armed_ms = now_ms; return; }
  if (st != S_ARMED) return;
  /* auto-trigger after one record length (samples x interval), kept within 10 .. 500 ms: few samples / fast time
     bases refresh quickly, long records get time for a real trigger without looking frozen */
  uint32_t to = (uint32_t)((float)cfg.n * (float)cfg.dec * DT_US / 4000.0f);   /* a quarter record */
  if (to < 10) to = 10;
  if (to > 100) to = 100;
  if (now_ms - armed_ms > to) force = true;       /* no trigger: show what is there */
}

bool scope_frame(const int16_t **b, uint32_t *start, uint32_t *n, bool *f) {
  if (st != S_DONE || cfg.pulse) return false;
  __disable_irq();
  send_start = wi;                                /* oldest sample: the next one that would be written */
  send_written = 0;
  send_sent = 0;
  filled = 0;
  st = S_SEND;                                    /* recording goes on behind the sender */
  __enable_irq();
  *b = mem;
  *start = send_start;
  *n = cfg.n;
  *f = forced;
  return true;
}

bool scope_pulse(scope_pulse_t *p) {
  if (st != S_DONE || !cfg.pulse) return false;
  if (!p_rdone) rise_copy();                      /* short pulse: the ring never wrapped */
  /* order the rings: rising window oldest-first into place, falling window likewise */
  static int16_t tmp[64];
  for (int pass = 0; pass < 2; pass++) {          /* rotate in place by `r` with a small buffer (juggling) */
    int16_t *a = pass ? &mem[FALL] : mem;
    uint32_t nn = PULSE_WIN, r = pass ? (p_fcount >= PULSE_WIN ? p_fw : 0) : wi;
    while (r) {
      uint32_t t = r < 64 ? r : 64;
      for (uint32_t i = 0; i < t; i++) tmp[i] = a[i];
      for (uint32_t i = 0; i < nn - t; i++) a[i] = a[i + t];
      for (uint32_t i = 0; i < t; i++) a[nn - t + i] = tmp[i];
      r -= t;
    }
  }
  p->rise = mem;
  p->fall = &mem[FALL];
  p->fall_n = p_fcount < PULSE_WIN ? p_fcount : PULSE_WIN;
  p->fall_start = p_fcount - p->fall_n;
  p->env = &mem[ENV0];
  p->env_n = e_n;
  p->env_bucket = 4 * e_bucket;                  /* in samples */
  p->end = p_end;
  p->count = p_cnt;
  p->mean = (int32_t)(p_sum / (int64_t)p_cnt);
  p->plateau = p_p4 == INT32_MIN ? (int32_t)(p_sum / (int64_t)p_cnt) : p_p4 / 4;
  p->clipped = p_clip;
  p->timeout = p_timeout;
  return true;
}

void scope_frame_sent(uint32_t count) { send_sent = count; }

void scope_frame_done(uint32_t now_ms) {
  __disable_irq();
  if (cfg.mode == SCOPE_SINGLE) running = false;
  if (st == S_SEND && running) {                  /* keep the history recorded while sending */
    st = filled >= need_pre() ? S_ARMED : S_FILL;
    force = false;
  } else {
    rearm();
  }
  p_clip = false;
  __enable_irq();
  armed_ms = now_ms;
}

uint32_t scope_roll_avail(void) { return rq_w - rq_r; }

uint32_t scope_roll_read(int16_t *out, uint32_t max) {
  uint32_t n = 0;
  while (n < max && rq_r != rq_w) { out[n++] = rollq[rq_r & (ROLLQ - 1)]; rq_r++; }
  return n;
}
