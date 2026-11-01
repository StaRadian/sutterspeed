/* Sutterspeed USB bootloader (first 12 KB of the flash, src/layout.h).
   After a reset it starts the firmware behind it, unless the firmware asked for an update (`dfu` command) or there
   is no valid firmware. Then it is a USB DFU 1.1 device (1209:0002, WinUSB through Microsoft OS 2.0 descriptors):
   block n (2 KB) goes to APP_BASE + n * 2 KB, each page is erased before its first write and read back after it.
   The first double word of the firmware (stack pointer + reset vector) is written last, at the end of the
   download, so an interrupted update leaves no half firmware that would be started: the bootloader stays. */
#include <string.h>
#include "board.h"
#include "layout.h"
#include "stm32c0xx_ll_bus.h"
#include "stm32c0xx_ll_system.h"
#include "tusb.h"

void clock_init(void);

static volatile uint32_t ms_ticks;
void SysTick_Handler(void) { ms_ticks++; }
uint32_t tusb_time_millis_api(void) { return ms_ticks; }

void USB_DRD_FS_IRQHandler(void) { tud_int_handler(0); }

/* ---------------------------------------------------------------- start the firmware / the ST bootloader */
static bool vector_ok(uint32_t sp, uint32_t pc) {
  return sp > RAM_START && sp <= RAM_TOP && (sp & 3) == 0 && (pc & 1) && pc > APP_BASE && pc < FLASH_TOP;
}

/* new stack pointer, then branch: in assembly, since C may keep `pc` on the old stack and read it after the switch */
__attribute__((naked, noreturn)) static void set_sp_and_go(uint32_t sp, uint32_t pc) {
  __asm volatile("msr msp, r0\n\tbx r1\n");
}

static void jump(uint32_t vtor, uint32_t sp, uint32_t pc) {   /* right after reset: peripherals untouched */
  SCB->VTOR = vtor;
  __DSB();
  set_sp_and_go(sp, pc);
}

/* ---------------------------------------------------------------- flash */
#define FLASH_ERRORS (FLASH_SR_OPERR | FLASH_SR_PROGERR | FLASH_SR_WRPERR | FLASH_SR_PGAERR | FLASH_SR_SIZERR | \
                      FLASH_SR_PGSERR | FLASH_SR_MISERR | FLASH_SR_FASTERR)

static void flash_wait(void) {
  while (FLASH->SR & (FLASH_SR_BSY1 | FLASH_SR_CFGBSY)) {}
}

static void flash_unlock(void) {
  if (FLASH->CR & FLASH_CR_LOCK) {
    FLASH->KEYR = 0x45670123;
    FLASH->KEYR = 0xCDEF89AB;
  }
}

static bool page_erase(uint32_t page) {
  flash_wait();
  FLASH->SR = FLASH_ERRORS;
  FLASH->CR = (FLASH->CR & ~FLASH_CR_PNB) | (page << FLASH_CR_PNB_Pos) | FLASH_CR_PER;
  FLASH->CR |= FLASH_CR_STRT;
  flash_wait();
  FLASH->CR &= ~(FLASH_CR_PER | FLASH_CR_PNB);
  return !(FLASH->SR & FLASH_ERRORS);
}

static bool program(uint32_t addr, const uint32_t w[2]) {
  flash_wait();
  FLASH->SR = FLASH_ERRORS;
  FLASH->CR |= FLASH_CR_PG;
  *(volatile uint32_t *)addr = w[0];
  __ISB();
  *(volatile uint32_t *)(addr + 4) = w[1];
  flash_wait();
  FLASH->CR &= ~FLASH_CR_PG;
  return !(FLASH->SR & FLASH_ERRORS) && *(volatile uint32_t *)addr == w[0] && *(volatile uint32_t *)(addr + 4) == w[1];
}

/* ---------------------------------------------------------------- DFU */
static uint32_t erased;          /* pages erased during this download, bit = page number */
static uint32_t head[2];         /* first double word of the firmware, written at the end */
static bool have_head;
static uint32_t last_activity, reboot_at;

uint32_t tud_dfu_get_timeout_cb(uint8_t alt, uint8_t state) {
  (void)alt;
  return state == DFU_DNBUSY ? 60 : 5;   /* page erase ~22 ms + 256 double words ~22 ms */
}

void tud_dfu_download_cb(uint8_t alt, uint16_t block, uint8_t const *data, uint16_t len) {
  (void)alt;
  last_activity = ms_ticks;
  uint32_t base = APP_BASE + (uint32_t)block * CFG_TUD_DFU_XFER_BUFSIZE;
  if (base + len > FLASH_TOP) {
    tud_dfu_finish_flashing(DFU_STATUS_ERR_ADDRESS);
    return;
  }
  uint8_t st = DFU_STATUS_OK;
  flash_unlock();
  for (uint32_t i = 0; i < len && st == DFU_STATUS_OK; i += 8) {
    uint32_t a = base + i, page = (a - FLASH_START) / FLASH_PAGE;
    if (!(erased & (1UL << page))) {
      erased |= 1UL << page;
      if (!page_erase(page)) st = DFU_STATUS_ERR_ERASE;
    }
    uint32_t w[2] = {0xFFFFFFFF, 0xFFFFFFFF};
    memcpy(w, data + i, len - i < 8 ? len - i : 8);
    if (a == APP_BASE) {
      head[0] = w[0];
      head[1] = w[1];
      have_head = true;
    } else if (st == DFU_STATUS_OK && !program(a, w)) {
      st = DFU_STATUS_ERR_VERIFY;
    }
  }
  FLASH->CR |= FLASH_CR_LOCK;
  tud_dfu_finish_flashing(st);
}

void tud_dfu_manifest_cb(uint8_t alt) {
  (void)alt;
  uint8_t st = DFU_STATUS_OK;
  if (!have_head || !vector_ok(head[0], head[1])) {
    st = DFU_STATUS_ERR_FIRMWARE;
  } else {
    flash_unlock();
    if (!program(APP_BASE, head)) st = DFU_STATUS_ERR_VERIFY;
    FLASH->CR |= FLASH_CR_LOCK;
  }
  have_head = false;
  erased = 0;
  if (st == DFU_STATUS_OK) reboot_at = ms_ticks + 1000;   /* the host normally sends DETACH before that */
  tud_dfu_finish_flashing(st);
}

void tud_dfu_detach_cb(void) { reboot_at = ms_ticks + 50; }

void tud_dfu_abort_cb(uint8_t alt) {
  (void)alt;
  erased = 0;
  have_head = false;
}

/* ---------------------------------------------------------------- main */
int main(void) {
  /* started by the ST bootloader after it wrote the flash ("leave"): its clocks, USB and remap are still set up,
     so restart from a clean reset */
  LL_APB2_GRP1_EnableClock(LL_APB2_GRP1_PERIPH_SYSCFG);
  if (LL_SYSCFG_GetRemapMemory() != LL_SYSCFG_REMAP_FLASH) NVIC_SystemReset();

  uint32_t req = BOOT_REQUEST;
  BOOT_REQUEST = 0;
  const uint32_t *app = (const uint32_t *)APP_BASE;
  if (req == BOOT_REQ_ROM) {
    LL_APB2_GRP1_EnableClock(LL_APB2_GRP1_PERIPH_SYSCFG);
    LL_SYSCFG_SetRemapMemory(LL_SYSCFG_REMAP_SYSTEMFLASH);
    const uint32_t *rom = (const uint32_t *)SYSMEM_BASE;
    jump(0, rom[0], rom[1]);
  }
  bool app_ok = vector_ok(app[0], app[1]);
  if (req != BOOT_REQ_DFU && app_ok) jump(APP_BASE, app[0], app[1]);

  clock_init();
  SysTick_Config(SYSCLK_HZ / 1000);
  LL_APB1_GRP1_EnableClock(LL_APB1_GRP1_PERIPH_USB);
  const tusb_rhport_init_t rh = {.role = TUSB_ROLE_DEVICE, .speed = TUSB_SPEED_FULL};
  tusb_init(0, &rh);

  while (1) {
    tud_task();
    uint32_t now = ms_ticks;
    if (reboot_at && (int32_t)(now - reboot_at) >= 0) NVIC_SystemReset();
    /* asked for an update but nothing came: back to the firmware after a minute */
    if (app_ok && !reboot_at && now - last_activity > 60000) NVIC_SystemReset();
  }
}
