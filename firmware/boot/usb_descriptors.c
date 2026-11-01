/* Bootloader USB descriptors: one DFU 1.1 interface (DFU mode) plus Microsoft OS 2.0 descriptors, so Windows 8.1+
   binds its own WinUSB driver to the device on the first plug-in: no driver to install. */
#include <string.h>
#include "stm32c0xx.h"
#include "tusb.h"

/* pid.codes test VID; the firmware (CDC) is 1209:0001, the bootloader gets its own PID so Windows keeps the
   two drivers apart */
#define USB_VID 0x1209
#define USB_PID 0x0002
#define VENDOR_MS 0x01   /* vendor request code for the Microsoft OS 2.0 descriptor set */

static tusb_desc_device_t const desc_device = {
    .bLength = sizeof(tusb_desc_device_t),
    .bDescriptorType = TUSB_DESC_DEVICE,
    .bcdUSB = 0x0210,    /* 2.1: has a BOS descriptor */
    .bDeviceClass = 0,
    .bDeviceSubClass = 0,
    .bDeviceProtocol = 0,
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

#define DFU_ATTRS (DFU_ATTR_CAN_DOWNLOAD | DFU_ATTR_MANIFESTATION_TOLERANT | DFU_ATTR_WILL_DETACH)
#define CONFIG_TOTAL_LEN (TUD_CONFIG_DESC_LEN + TUD_DFU_DESC_LEN(1))

static uint8_t const desc_configuration[] = {
    TUD_CONFIG_DESCRIPTOR(1, 1, 0, CONFIG_TOTAL_LEN, 0x00, 100),
    TUD_DFU_DESCRIPTOR(0, 1, 4, DFU_ATTRS, 1000, CFG_TUD_DFU_XFER_BUFSIZE),
};

uint8_t const *tud_descriptor_configuration_cb(uint8_t index) {
  (void)index;
  return desc_configuration;
}

/* ---------------------------------------------------------------- Microsoft OS 2.0 (whole device -> WinUSB) */
typedef struct TU_ATTR_PACKED {
  uint8_t set_header[10];
  uint8_t compat_id[20];
  uint8_t prop_header[8];   /* length, type, data type (REG_MULTI_SZ), name length */
  uint16_t name[21];        /* UTF-16 */
  uint16_t data_len;
  uint16_t data[40];
} ms_os_20_t;

static ms_os_20_t const desc_ms_os_20 = {
    .set_header = {U16_TO_U8S_LE(10), U16_TO_U8S_LE(MS_OS_20_SET_HEADER_DESCRIPTOR), U32_TO_U8S_LE(0x06030000),
                   U16_TO_U8S_LE(sizeof(ms_os_20_t))},
    .compat_id = {U16_TO_U8S_LE(20), U16_TO_U8S_LE(MS_OS_20_FEATURE_COMPATBLE_ID), 'W', 'I', 'N', 'U', 'S', 'B'},
    .prop_header = {U16_TO_U8S_LE(sizeof(ms_os_20_t) - 30), U16_TO_U8S_LE(MS_OS_20_FEATURE_REG_PROPERTY),
                    U16_TO_U8S_LE(7), U16_TO_U8S_LE(42)},
    .name = u"DeviceInterfaceGUIDs",
    .data_len = 80,
    .data = u"{3F6A1C52-8E4B-4D7A-9C21-5B0E7D93A4F6}\0",
};
TU_VERIFY_STATIC(sizeof(ms_os_20_t) == 162, "MS OS 2.0 descriptor size");

#define BOS_TOTAL_LEN (TUD_BOS_DESC_LEN + TUD_BOS_MICROSOFT_OS_DESC_LEN)
static uint8_t const desc_bos[] = {
    TUD_BOS_DESCRIPTOR(BOS_TOTAL_LEN, 1),
    TUD_BOS_MS_OS_20_DESCRIPTOR(sizeof(ms_os_20_t), VENDOR_MS),
};

uint8_t const *tud_descriptor_bos_cb(void) { return desc_bos; }

bool tud_vendor_control_xfer_cb(uint8_t rhport, uint8_t stage, tusb_control_request_t const *request) {
  if (stage != CONTROL_STAGE_SETUP) return true;
  if (request->bmRequestType_bit.type == TUSB_REQ_TYPE_VENDOR && request->bRequest == VENDOR_MS && request->wIndex == 7)
    return tud_control_xfer(rhport, request, (void *)(uintptr_t)&desc_ms_os_20, sizeof(desc_ms_os_20));
  return false;
}

/* ---------------------------------------------------------------- strings */
static char const *const string_desc[] = {
    (const char[]){0x09, 0x04},  /* 0: English */
    "sutterspeed",               /* 1: manufacturer */
    "Sutterspeed bootloader",    /* 2: product */
    NULL,                        /* 3: serial (from the chip UID) */
    "Sutterspeed firmware",      /* 4: DFU alt 0 */
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
