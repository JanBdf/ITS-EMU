# AC6 fixtures

Firmware images of the [ITS-BRD-VSC](https://github.com/Transport-Protocol/ITS-BRD-VSC)
programs, built with Arm Compiler 6 (Debug) by `tools/build-fixtures.sh`.

Only the course demo programs are checked in (the font table is
`crates/itsboard/assets/fonts.bin`). They are built from code that is

    Copyright (c) 2022, HAW Hamburg, Labor fuer technische Informatik
    BSD 2-Clause License

and contain parts of the STM32CubeF4 HAL (Copyright STMicroelectronics,
BSD 3-Clause) and CMSIS (Copyright Arm Limited, Apache-2.0).

The own test programs (sources in `tests/fixtures/ac6-src`, among them the
GUI's tutorial program) are built with the same library.

Lab solutions (A1ASM, LauflichtASM, StoppuhrASM, …) are not published; tests
that need them are skipped when the files are missing.
