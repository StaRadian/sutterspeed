cmake_minimum_required(VERSION 3.22)
set(CMAKE_TOOLCHAIN_FILE ${CMAKE_CURRENT_LIST_DIR}/cmake/gcc-arm-none-eabi.cmake)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE MinSizeRel)
endif()
project(sutterspeed C ASM)
set(CMAKE_C_STANDARD 11)

set(LIB ${CMAKE_CURRENT_LIST_DIR}/lib)
set(TUSB ${LIB}/tinyusb/src)

add_executable(sutterspeed
  src/main.c
  src/clock.c
  src/acq.c
  src/meas.c
  src/scope.c
  src/cli.c
  src/usb_descriptors.c
  ${LIB}/cmsis-device-c0/Source/Templates/system_stm32c0xx.c
  ${LIB}/cmsis-device-c0/Source/Templates/gcc/startup_stm32c071xx.s
  ${TUSB}/tusb.c
  ${TUSB}/common/tusb_fifo.c
  ${TUSB}/device/usbd.c
  ${TUSB}/class/cdc/cdc_device.c
  ${TUSB}/portable/st/stm32_fsdev/dcd_stm32_fsdev.c
  ${TUSB}/portable/st/stm32_fsdev/fsdev_common.c
)

target_include_directories(sutterspeed PRIVATE
  src
  ${LIB}/cmsis-core/CMSIS/Core/Include
  ${LIB}/cmsis-device-c0/Include
  ${LIB}/stm32c0xx-hal-driver/Inc
  ${TUSB}
)

set_source_files_properties(src/meas.c src/scope.c PROPERTIES COMPILE_OPTIONS "-O2")   # hot path

# the firmware runs behind the 12 KB bootloader (src/layout.h)
target_compile_definitions(sutterspeed PRIVATE STM32C071xx USE_FULL_LL_DRIVER CFG_TUSB_MCU=OPT_MCU_STM32C0
  VECT_TAB_OFFSET=0x3000U)
target_link_options(sutterspeed PRIVATE
  -L${CMAKE_CURRENT_LIST_DIR}/linker -T${CMAKE_CURRENT_LIST_DIR}/linker/app.ld
  -Wl,-Map=${CMAKE_CURRENT_BINARY_DIR}/sutterspeed.map)
target_link_libraries(sutterspeed PRIVATE m)

# USB bootloader (boot/): first 12 KB of the flash, DFU with WinUSB descriptors
add_executable(boot
  boot/boot.c
  boot/usb_descriptors.c
  src/clock.c
  ${LIB}/cmsis-device-c0/Source/Templates/system_stm32c0xx.c
  ${LIB}/cmsis-device-c0/Source/Templates/gcc/startup_stm32c071xx.s
  ${TUSB}/tusb.c
  ${TUSB}/common/tusb_fifo.c
  ${TUSB}/device/usbd.c
  ${TUSB}/class/dfu/dfu_device.c
  ${TUSB}/portable/st/stm32_fsdev/dcd_stm32_fsdev.c
  ${TUSB}/portable/st/stm32_fsdev/fsdev_common.c
)
target_include_directories(boot PRIVATE
  boot
  src
  ${LIB}/cmsis-core/CMSIS/Core/Include
  ${LIB}/cmsis-device-c0/Include
  ${LIB}/stm32c0xx-hal-driver/Inc
  ${TUSB}
)
target_compile_definitions(boot PRIVATE STM32C071xx USE_FULL_LL_DRIVER CFG_TUSB_MCU=OPT_MCU_STM32C0)
target_link_options(boot PRIVATE
  -L${CMAKE_CURRENT_LIST_DIR}/linker -T${CMAKE_CURRENT_LIST_DIR}/linker/boot.ld
  -Wl,-Map=${CMAKE_CURRENT_BINARY_DIR}/boot.map)

# sutterspeed.hex / .bin = bootloader + firmware (the whole flash image, for the ST bootloader or a programmer);
# app.bin = the firmware alone
set(B ${CMAKE_CURRENT_BINARY_DIR})
add_custom_command(TARGET sutterspeed POST_BUILD
  COMMAND ${CMAKE_OBJCOPY} -O binary --pad-to 0x08003000 --gap-fill 0xFF $<TARGET_FILE:boot> ${B}/boot.bin
  COMMAND ${CMAKE_OBJCOPY} -O binary $<TARGET_FILE:sutterspeed> ${B}/app.bin
  COMMAND ${CMAKE_COMMAND} -E cat ${B}/boot.bin ${B}/app.bin > ${B}/sutterspeed.bin
  COMMAND ${CMAKE_OBJCOPY} -I binary -O ihex --change-addresses 0x08000000 ${B}/sutterspeed.bin ${B}/sutterspeed.hex
  COMMAND ${CMAKE_SIZE} $<TARGET_FILE:boot> $<TARGET_FILE:sutterspeed>)
add_dependencies(sutterspeed boot)
