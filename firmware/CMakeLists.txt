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

target_compile_definitions(sutterspeed PRIVATE STM32C071xx USE_FULL_LL_DRIVER CFG_TUSB_MCU=OPT_MCU_STM32C0)
target_link_options(sutterspeed PRIVATE
  -T${CMAKE_CURRENT_LIST_DIR}/linker/STM32C071G8_FLASH.ld
  -Wl,-Map=${CMAKE_CURRENT_BINARY_DIR}/sutterspeed.map)
target_link_libraries(sutterspeed PRIVATE m)

add_custom_command(TARGET sutterspeed POST_BUILD
  COMMAND ${CMAKE_OBJCOPY} -O binary $<TARGET_FILE:sutterspeed> ${CMAKE_CURRENT_BINARY_DIR}/sutterspeed.bin
  COMMAND ${CMAKE_OBJCOPY} -O ihex   $<TARGET_FILE:sutterspeed> ${CMAKE_CURRENT_BINARY_DIR}/sutterspeed.hex
  COMMAND ${CMAKE_SIZE} $<TARGET_FILE:sutterspeed>)
