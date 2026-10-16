#!/bin/sh
# Builds the emulator self-test firmware. Needs clang + ld.lld (any recent LLVM).
set -e
cd "$(dirname "$0")"
for src in *.c; do
  clang --target=thumbv7em-none-eabihf -mcpu=cortex-m4 -mfpu=fpv4-sp-d16 -mfloat-abi=hard \
        -O1 -g -ffile-prefix-map="$PWD"=. -ffp-contract=off -ffreestanding -nostdlib -fno-builtin -fuse-ld=lld -Wl,-T,link.ld \
        -o "${src%.c}.elf" "$src"
done
