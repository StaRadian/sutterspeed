/* USB descriptors: one CDC-ACM (virtual COM port). Based on the TinyUSB cdc_msc example. */
#include <string.h>
#include "stm32c0xx.h"
#include "tusb.h"

/* pid.codes test VID/PID (1209:0001) - fine for a personal device, not for products */
#define USB_VID 0x1209
#define USB_PID 0x0001

static tusb_desc_device_t const desc_device = {
    .bLength = sizeof(tusb_desc_device_t),
    .bDescriptorType = TUSB_DESC_DEVICE,
    .bcdUSB = 0x0200,
    .bDeviceClass = TUSB_CLASS_MISC,
    .bDeviceSubClass = MISC_SUBCLASS_COMMON,
    .bDeviceProtocol = MISC_PROTOCOL_IAD,
    .bMaxPacketSize0 = CFG_TUD_ENDPOINT0_SIZE,
    .idVendor = USB_VID,
    .idProduct = USB_PID,
    .bcdDevice = 0x0100,
    .iManufacturer = 1,
    .iProduct = 2,
    .iSerialNumber = 3,
    .bNumConfigurations = 1,
};

uint8_t const *tud_descriptor_device_cb(void) { return (uint8_t const *)&desc_device; }

enum { ITF_NUM_CDC = 0, ITF_NUM_CDC_DATA, ITF_NUM_TOTAL };
#define EPNUM_CDC_NOTIF 0x81
#define EPNUM_CDC_OUT   0x02
#define EPNUM_CDC_IN    0x82
#define CONFIG_TOTAL_LEN (TUD_CONFIG_DESC_LEN + TUD_CDC_DESC_LEN)

static uint8_t const desc_configuration[] = {
    TUD_CONFIG_DESCRIPTOR(1, ITF_NUM_TOTAL, 0, CONFIG_TOTAL_LEN, 0x00, 100),
    TUD_CDC_DESCRIPTOR(ITF_NUM_CDC, 4, EPNUM_CDC_NOTIF, 8, EPNUM_CDC_OUT, EPNUM_CDC_IN, 64),
};

uint8_t const *tud_descriptor_configuration_cb(uint8_t index) {
  (void)index;
  return desc_configuration;
}

static char const *const string_desc[] = {
    (const char[]){0x09, 0x04},  /* 0: English */
    "sutterspeed",               /* 1: manufacturer */
    "Sutterspeed shutter tester", /* 2: product */
    NULL,                        /* 3: serial (from the chip UID) */
    "Sutterspeed CDC",           /* 4: CDC interface */
};

static uint16_t desc_str[33];

uint16_t const *tud_descriptor_string_cb(uint8_t index, uint16_t langid) {
  (void)langid;
  uint8_t n;
  if (index == 0) {
    memcpy(&desc_str[1], string_desc[0], 2);
    n = 1;
  } else if (index == 3) {
    const uint32_t *uid = (const uint32_t *)UID_BASE;
    static const char hex[] = "0123456789ABCDEF";
    n = 0;
    for (int w = 0; w < 3; w++)
      for (int s = 28; s >= 0; s -= 4) desc_str[1 + n++] = hex[(uid[w] >> s) & 0xF];
  } else {
    if (index >= sizeof(string_desc) / sizeof(string_desc[0])) return NULL;
    const char *str = string_desc[index];
    n = (uint8_t)strlen(str);
    if (n > 32) n = 32;
    for (uint8_t i = 0; i < n; i++) desc_str[1 + i] = str[i];
  }
  desc_str[0] = (uint16_t)((TUSB_DESC_STRING << 8) | (2 * n + 2));
  return desc_str;
}
