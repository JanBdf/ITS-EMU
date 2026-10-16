# AC6 fixtures

Firmware images of the [ITS-BRD-VSC](https://github.com/Transport-Protocol/ITS-BRD-VSC)
programs, built with Arm Compiler 6 (Debug) by `tools/build-fixtures.sh`.

Only the course demo programs and `fonts.bin` are checked in. They are
built from code that is

    Copyright (c) 2022, HAW Hamburg, Labor fuer technische Informatik
    BSD 2-Clause License

and contain parts of the STM32CubeF4 HAL (Copyright STMicroelectronics,
BSD 3-Clause) and CMSIS (Copyright Arm Limited, Apache-2.0).

Lab solutions (A1ASM, LauflichtASM, StoppuhrASM, …) are not published; tests
that need them are skipped when the files are missing.
