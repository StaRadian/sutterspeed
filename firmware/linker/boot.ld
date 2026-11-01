/* STM32C071G8 USB bootloader: the first 12 KB of the flash (src/layout.h). */
ENTRY(Reset_Handler)

_Min_Heap_Size = 0x0;
_Min_Stack_Size = 0x400;

MEMORY
{
  /* the last 16 bytes of the RAM are left out: request word from the firmware to the bootloader (src/layout.h) */
  RAM    (xrw)    : ORIGIN = 0x20000000,   LENGTH = 24K - 16
  FLASH  (rx)     : ORIGIN = 0x8000000,    LENGTH = 12K
}

_estack = ORIGIN(RAM) + LENGTH(RAM);

INCLUDE sections.ld
