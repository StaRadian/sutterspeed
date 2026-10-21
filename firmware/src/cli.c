/* USB serial interface: text commands, oscilloscope frames / pulses as hex lines. No float printf
   (newlib-nano): numbers are formatted from scaled integers. */
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "board.h"
#include "cli.h"
#include "meas.h"
#include "scope.h"
#include "tusb.h"

#define FW_VERSION "0.3.2"

extern volatile uint32_t acq_overruns, acq_isr_us_max, acq_isr_us_last, acq_isr_calls;
uint32_t millis(void);
void request_bootloader(void);

/* ---------------------------------------------------------------- output */
/* queue text for the host; bulk data leaves it unflushed so it goes out in full 64-byte packets */
static void out_q(const char *s, uint32_t len) {
  while (len) {
    if (!tud_cdc_connected()) return;            /* nobody listening: drop */
    uint32_t av = tud_cdc_write_available();
    if (av == 0) { tud_task(); continue; }
    uint32_t n = len < av ? len : av;
    tud_cdc_write(s, n);
    s += n; len -= n;
  }
}

static void out(const char *s, uint32_t len) {
  out_q(s, len);
  tud_cdc_write_flush();
}

static void outs(const char *s) { out(s, (uint32_t)strlen(s)); }

static void outf(const char *fmt, ...) __attribute__((format(printf, 1, 2)));
static void outf(const char *fmt, ...) {
  char b[160];
  va_list ap;
  va_start(ap, fmt);
  int n = vsnprintf(b, sizeof b, fmt, ap);
  va_end(ap);
  if (n > 0) out(b, (uint32_t)(n < (int)sizeof b ? n : (int)sizeof b - 1));
}

/* fixed-point text: value with `dec` decimals */
static const char *fx(char *b, float v, int dec) {
  int32_t sc = dec == 0 ? 1 : dec == 1 ? 10 : dec == 2 ? 100 : 1000;
  int32_t x = (int32_t)(v * (float)sc + (v < 0 ? -0.5f : 0.5f));
  const char *sg = x < 0 ? "-" : "";
  if (x < 0) x = -x;
  if (dec == 0) sprintf(b, "%s%ld", sg, (long)x);
  else sprintf(b, "%s%ld.%0*ld", sg, (long)(x / sc), dec, (long)(x % sc));
  return b;
}

/* ---------------------------------------------------------------- commands */
static bool lux_stream;

/* ---------------------------------------------------------------- oscilloscope */
/* sample -> 3 characters, 6 bits each, '0' + (0..63) (ASCII 48..111: no '#', no line breaks) */
static char *enc3(char *p, int16_t v) {
  uint16_t u = (uint16_t)v;
  p[0] = (char)('0' + (u >> 12));
  p[1] = (char)('0' + ((u >> 6) & 63));
  p[2] = (char)('0' + (u & 63));
  return p + 3;
}

static void scope_print_cfg(void) {
  scope_cfg_t c;
  bool run;
  scope_get(&c, &run);
  outf("scope %s dec=%u n=%u post=%lu lvl=%ld edge=%c mode=%c roll=%d pulse=%d maxp=%lu tf=%u\r\n", run ? "run" : "stop",
       c.dec, c.n, (unsigned long)c.post, (long)c.level, c.falling ? 'f' : 'r', "ansf"[c.mode], c.roll,
       c.pulse, (unsigned long)c.maxp_ms, c.tfk);
}

/* scope set <dec> <n> <post> <level> <r|f> <a|n|s> <roll 0|1> <pulse 0|1> <maxp_ms> */
static void cmd_scope(char *arg) {
  char *w = strtok(arg, " ");
  if (!w) { scope_print_cfg(); return; }
  if (!strcmp(w, "on") || !strcmp(w, "off")) { /* always on since 0.2 */ }
  else if (!strcmp(w, "run")) scope_run(true);
  else if (!strcmp(w, "stop")) scope_run(false);
  else if (!strcmp(w, "set")) {
    scope_cfg_t c;
    bool run;
    scope_get(&c, &run);
    char *t;
    if ((t = strtok(NULL, " "))) c.dec = (uint16_t)atoi(t);
    if ((t = strtok(NULL, " "))) c.n = (uint16_t)atoi(t);
    if ((t = strtok(NULL, " "))) c.post = (uint32_t)strtoul(t, NULL, 10);
    if ((t = strtok(NULL, " "))) c.level = atol(t);
    if ((t = strtok(NULL, " "))) c.falling = *t == 'f';
    if ((t = strtok(NULL, " "))) c.mode = *t == 'n' ? SCOPE_NORMAL : *t == 's' ? SCOPE_SINGLE : *t == 'f' ? SCOPE_FREE : SCOPE_AUTO;
    if ((t = strtok(NULL, " "))) c.roll = *t == '1';
    if ((t = strtok(NULL, " "))) c.pulse = *t == '1';
    if ((t = strtok(NULL, " "))) c.maxp_ms = (uint32_t)strtoul(t, NULL, 10);
    if ((t = strtok(NULL, " "))) c.tfk = (uint8_t)atoi(t);
    scope_config(&c);
  } else {
    outs("scope [run|stop|set dec n post level r|f a|n|s|f roll pulse maxp_ms tf]\r\n");
    return;
  }
  scope_print_cfg();
}

static void hex_lines(const int16_t *b, uint32_t start, uint32_t ring, uint32_t n, bool progress) {
  char line[64 * 3 + 2];
  for (uint32_t i = 0; i < n;) {
    char *p = line;
    for (int j = 0; j < 64 && i < n; j++, i++) p = enc3(p, b[(start + i) % ring]);
    *p++ = '\r'; *p++ = '\n';
    if (progress) scope_frame_sent(i);            /* these are in the line buffer: the ring slots may be reused */
    out_q(line, (uint32_t)(p - line));
  }
}

static void scope_task(void) {
  static uint32_t last_roll;
  uint32_t now = millis();
  scope_poll(now);
  char t[24], u[24];
  const int16_t *b;
  uint32_t st0, n;
  bool forced;
  scope_cfg_t c;
  bool run;
  scope_pulse_t pl;
  if (scope_frame(&b, &st0, &n, &forced)) {
    scope_get(&c, &run);
    outf("#SCOPE enc=3 n=%lu dec=%u trig=%ld lvl=%ld forced=%d dt_us=%s cpl=%s\r\n", (unsigned long)n, c.dec,
         (long)(n - 1) - (long)c.post, (long)c.level, forced, fx(t, DT_US, 3), fx(u, COUNTS_PER_LUX, 2));
    hex_lines(b, st0, n, n, true);
    outs("#SEND\r\n");
    scope_frame_done(millis());
  } else if (scope_pulse(&pl)) {
    scope_get(&c, &run);
    outf("#PULSE enc=3 dt_us=%s cpl=%s win=%u pre=%u fn=%lu fstart=%lu end=%lu count=%lu\r\n", fx(t, DT_US, 3),
         fx(u, COUNTS_PER_LUX, 2), PULSE_WIN, PULSE_PRE, (unsigned long)pl.fall_n, (unsigned long)pl.fall_start,
         (unsigned long)pl.end, (unsigned long)pl.count);
    outf("#P mean=%ld plat=%ld en=%lu eb=%lu clip=%d to=%d lvl=%ld\r\n", (long)pl.mean, (long)pl.plateau,
         (unsigned long)pl.env_n, (unsigned long)pl.env_bucket, pl.clipped, pl.timeout, (long)c.level);
    hex_lines(pl.rise, 0, PULSE_WIN, PULSE_WIN, false);
    outs("#F\r\n");
    hex_lines(pl.fall, 0, PULSE_WIN, pl.fall_n, false);
    outs("#E\r\n");
    hex_lines(pl.env, 0, 2 * PULSE_ENV, 2 * pl.env_n, false);
    outs("#PEND\r\n");
    scope_frame_done(millis());
  }
  /* roll stream: full lines as soon as they are there, the rest at least every 20 ms (up to ~50 kS/s) */
  for (int lines = 0; lines < 32; lines++) {
    uint32_t av = scope_roll_avail();
    if (av == 0 || (av < 64 && now - last_roll < 20)) break;
    last_roll = now;
    int16_t r[64];
    uint32_t k = scope_roll_read(r, 64);
    char line[3 + 64 * 3 + 2] = "#r ";               /* "#r": 3-character samples */
    char *p = line + 3;
    for (uint32_t i = 0; i < k; i++) p = enc3(p, r[i]);
    *p++ = '\r'; *p++ = '\n';
    out(line, (uint32_t)(p - line));
  }
}

static void exec(char *line) {
  char *arg = strchr(line, ' ');
  if (arg) { *arg++ = 0; while (*arg == ' ') arg++; }
  char a[24], b[24];

  if (!strcmp(line, "help") || !strcmp(line, "?")) {
    outs("commands:\r\n"
         "  scope ...     oscilloscope / pulse measurement (scope, scope run|stop|set ...)\r\n"
         "  lux [on|off]  current brightness (live stream, 10 per second, with 'on')\r\n"
         "  raw           raw ADC values (PA0 x1, PA1 x11, PA2 x121)\r\n"
         "  zero          cover the sensor first: learn dark offsets\r\n"
         "  gain          self-calibrated stage gains\r\n"
         "  dfu           reboot into the ST USB DFU bootloader (firmware update)\r\n"
         "  ver           firmware version and interrupt load\r\n");
  } else if (!strcmp(line, "lux")) {
    if (arg && !strcmp(arg, "on")) lux_stream = true;
    else if (arg && !strcmp(arg, "off")) lux_stream = false;
    else outf("%s lx\r\n", fx(a, meas_level_take(), 1));
  } else if (!strcmp(line, "raw")) {
    uint16_t r[3];
    meas_raw(r);
    outf("PA0 %u  PA1 %u  PA2 %u\r\n", r[0], r[1], r[2]);
  } else if (!strcmp(line, "zero")) {
    meas_zero();
    while (meas_zero_busy()) tud_task();
    outf("dark offsets: %ld %ld %ld\r\n", (long)meas_cal.o0, (long)meas_cal.o1, (long)meas_cal.o2);
  } else if (!strcmp(line, "gain")) {
    float g[2];
    meas_gains(g);
    outf("stage2 x%s  stage3 x%s  (nominal x11 / x121)\r\n", fx(a, g[0], 3), fx(b, g[1], 2));
  } else if (!strcmp(line, "scope")) {
    cmd_scope(arg ? arg : (char[]){0});
  } else if (!strcmp(line, "dfu")) {
    outs("rebooting into DFU...\r\n");
    uint32_t t0 = millis();
    while (millis() - t0 < 100) tud_task();
    request_bootloader();
  } else if (!strcmp(line, "ver")) {
    outf("sutterspeed fw " FW_VERSION "  scan %sus  overruns %lu  isr %lu/%lu us (budget 1280)  calls %lu\r\n",
         fx(a, DT_US, 2), (unsigned long)acq_overruns, (unsigned long)acq_isr_us_last, (unsigned long)acq_isr_us_max,
         (unsigned long)acq_isr_calls);
    acq_isr_us_max = 0;                          /* max since the previous 'ver' */
  } else if (*line) {
    outf("unknown command '%s' (try 'help')\r\n", line);
  }
}

/* ---------------------------------------------------------------- main-loop task */
void cli_task(void) {
  static char line[48];
  static uint8_t len;
  static uint32_t last_lux;
  static bool was_connected;

  bool conn = tud_cdc_connected();
  if (conn && !was_connected) outs("\r\nsutterspeed " FW_VERSION " light scope ready ('help' for commands)\r\n");
  was_connected = conn;

  meas_task();
  scope_task();

  while (tud_cdc_available()) {
    char ch = (char)tud_cdc_read_char();
    if (ch == '\r' || ch == '\n') {
      if (len) { outs("\r\n"); line[len] = 0; exec(line); len = 0; }
    } else if ((ch == '\b' || ch == 0x7f) && len) {
      len--; outs("\b \b");
    } else if (ch >= ' ' && len < sizeof line - 1) {
      line[len++] = ch; out(&ch, 1);
    }
  }

  if (lux_stream && millis() - last_lux >= 100) {   /* 10 Hz live level */
    char a[24];
    last_lux = millis();
    outf("lux %s\r\n", fx(a, meas_level_take(), 1));
  }
}
