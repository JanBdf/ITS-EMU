# Vendored dependencies

## unicorn-engine-sys 2.1.5

The C sources of [Unicorn](https://www.unicorn-engine.org/) as published on
crates.io (GPL-2.0, see `unicorn-engine-sys/COPYING`), used through
`[patch.crates-io]` in the workspace `Cargo.toml`. Changes for speed
(`unicorn-engine-sys.patch`, against the crates.io sources):

- No exit check after every guest load and store. Unicorn called a helper
  (which writes all TCG globals back to memory) after each access so that an
  invalid access could leave the translation block. Invalid stores now leave
  it right away with `cpu_loop_exit_restore`, as invalid loads already did.
- The exit check at the start of each translation block is QEMU's inline
  load and branch instead of the same helper call. The PC is set to the
  block's start in `cpu_tb_exec` also when code hooks exist.
- A Cortex-M4 when one is asked for (`UC_CTL_CPU_MODEL`). Unicorn made every
  M-class CPU a Cortex-M33, whose Security Extension sends every function
  return (BX LR, POP {PC}) out of the JIT to the main loop to check for
  FNC_RETURN/EXC_RETURN, and whose v8-M stack limit checks run on every SP
  change.
- Writing xPSR/IPSR rebuilds the cached TB flags, which include handler mode
  (the emulator enters and leaves exceptions by writing xPSR); QEMU then
  recognizes EXC_RETURN only in handler mode, as on the M4.
- The guest TLB index folds in higher address bits (64-bit x86 hosts, C and
  JIT): with the page number alone, the first pages of flash, SRAM and the
  peripherals shared entries, so alternating accesses (literal pool, stack)
  evicted each other and all took the slow path.
- `find_memory_mapping`, asked on every slow-path access (all MMIO), caches
  its answer per 1 KiB page until the memory map changes.
- Only the Arm guest: the other targets' sources are removed (`uc.c` still
  includes their `unicorn.h`).

To update: unpack the new crate here, remove `qemu/target/*` except `arm`
(keeping each `unicorn.h`) and apply the patch.
