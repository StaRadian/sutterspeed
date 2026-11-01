/* STM32C071G8 firmware: 64 KB flash minus the 12 KB bootloader in front (src/layout.h), 24 KB RAM. */
ENTRY(Reset_Handler)

_Min_Heap_Size = 0x0;
_Min_Stack_Size = 0x800;

MEMORY
{
  /* the last 16 bytes of the RAM are left out: request word from the firmware to the bootloader (src/layout.h) */
  RAM    (xrw)    : ORIGIN = 0x20000000,   LENGTH = 24K - 16
  FLASH  (rx)     : ORIGIN = 0x8003000,    LENGTH = 64K - 12K
}

_estack = ORIGIN(RAM) + LENGTH(RAM);

INCLUDE sections.ld
