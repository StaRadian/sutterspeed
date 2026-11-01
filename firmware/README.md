# sutterspeed firmware

STM32C071G8 · TinyUSB CDC (virtual COM port, VID:PID 1209:0001) · TIM3-triggered 3-channel ADC scan every 2.5 µs
(400 kscan/s), DMA into a circular buffer of 2 × 512 scans; each half (1.28 ms) is processed in the DMA interrupt.

## Build (STM32CubeCLT)

```
cmake -S . -B build -G Ninja
cmake --build build
```

Output: `build/sutterspeed.hex / .bin` = the whole flash image (bootloader + firmware), `build/app.bin` = the firmware
alone, `build/boot.elf`, `build/sutterspeed.elf`. The libraries in `lib/` are git submodules
(`git submodule update --init` if the folders are empty).

## Flash layout and USB bootloader

| flash | |
| --- | --- |
| `0x08000000` – `0x08002FFF` (12 KB) | USB bootloader (`boot/`) |
| `0x08003000` – `0x0800FFFF` (52 KB) | firmware (vector table at `0x08003000`) |

The bootloader starts the firmware unless the firmware asked for an update (`dfu` command) or there is no valid
firmware. Then it is a USB DFU 1.1 device, VID:PID 1209:0002, with Microsoft OS 2.0 descriptors: Windows 8.1+ binds
its own WinUSB driver, nothing to install. It writes the firmware in 2 KB blocks (erase, write, read back) and writes
the firmware's first double word (stack pointer, reset vector) last, so an interrupted update leaves the board in the
bootloader, ready for the next try. It returns to the firmware after a minute without a download.
The firmware's request goes across the reset in the last 16 bytes of the RAM, which both linker scripts leave out
(`src/layout.h`). A hard fault in the firmware also restarts into the bootloader.

`dfu rom` starts the ST system bootloader instead (DfuSe, 0483:DF11); the bootloader itself can only be replaced
through it or SWD.

## Flash

- From the viewer: chip icon → *Update from GitHub* or pick the `.hex`, or `sutterspeed-viewer --flash build/sutterspeed.hex`.
  The board restarts into its bootloader (`dfu` command) and the firmware part of the image is written over USB.
  Boards without the bootloader (firmware before 0.4.0) restart into the ST bootloader and get the whole image.
  A blank chip starts the ST bootloader by itself (empty-flash check, factory option bytes), so it is written the
  same way: plug it in, then *Update from GitHub* or `--flash`.
  When the ST bootloader has no driver on the PC, the viewer binds Windows' own WinUSB to it
  (it restarts itself as administrator once: `--install-driver`, Windows asks for permission).
- ST-LINK V3 + Tag-Connect TC2030-CTX-NL-STDC14 on J2:
  `STM32_Programmer_CLI -c port=SWD -w build/sutterspeed.hex -v -rst`

## Source

| file | |
| --- | --- |
| `acq.c` | TIM3 + ADC + DMA set-up, the interrupt that hands each half buffer on |
| `meas.c` | dark offsets (`zero`), learned stage gains, merging one scan into one value (stage-3 LSBs, 15.02 per lx) |
| `scope.c` | scope engine: decimation by any integer, trigger state machine (auto / normal / single / free run), roll stream, pulse capture |
| `cli.c` | command line and frame output |
| `main.c`, `clock.c`, `usb_descriptors.c` | main loop and restart into the bootloader, 48 MHz HSI48 clock (USB trimmed by CRS), USB descriptors |
| `layout.h` | flash layout and the firmware → bootloader request word |
| `../boot/` | USB bootloader: DFU download into the firmware area, WinUSB descriptors |

Hot code runs from RAM; the hot loops are split into short passes because the M0+ spills registers easily.

## Serial protocol

Commands (one per line):

```
scope set <dec> <n> <post> <level> <r|f> <a|n|s|f> <roll 0|1> <pulse 0|1> <maxp_ms> <tf> <minw>
scope run | scope stop
adc [scans]    raw channel statistics and stage ratios
dfu [rom]      restart into the USB bootloader (rom: the ST system bootloader)
lux [on|off]   raw   zero   gain   ver   help
```

- `dec` scans averaged per sample (1..4096), `n` samples per frame (16..4000), `post` samples after the trigger
  (may exceed `n`: trigger delay), `level` in counts (15.02 per lx), edge, mode (auto / normal / single / free run),
  `roll` streams every sample, `pulse` selects the shutter measurement, `maxp_ms` the longest pulse,
  `tf` a trigger filter `y += (x - y) / 2^tf` (in the shutter measurement two such stages),
  `minw` the minimum width in samples: a crossing that goes back sooner is no trigger (0 = off).
- A changed level or edge alone is applied in place, without restarting the capture.

Output:

- frame: `#SCOPE enc=3 n= dec= trig= lvl= forced= dt_us= cpl=`, sample lines, `#SEND`
- roll: `#r <samples>`
- pulse: `#PULSE enc=3 dt_us= cpl= win= pre= fn= fstart= end= count=`, `#P mean= plat= en= eb= clip= to= lvl=`,
  the rising window, `#F`, the falling window, `#E`, the min/max envelope, `#PEND`

Samples are 3 characters each (`'0'` + 6 bits); values above 16384 counts are stored in steps of 32.

## Host test

`test/test_pulse.c` feeds synthetic light pulses (125 µs .. 2 s, dim / bright / clipped, a noisy front end) and
decimation ramps through `scope.c` and checks the result. Built with MSVC from `test/`:

```
copy ..\src\scope.c stub\scope_ut.c & copy ..\src\meas.c stub\meas_ut.c
cl /nologo /O2 /I stub /I ..\src test_pulse.c stub\scope_ut.c stub\meas_ut.c /Fe:test_pulse.exe
```
