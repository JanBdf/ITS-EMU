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
  change. This is also a behaviour change: instructions and registers that
  only ARMv8-M has (MSPLIM/PSPLIM, TT, SG, ...) are undefined now, as on the
  STM32F429.
- Writing xPSR/IPSR rebuilds the cached TB flags, which include handler mode
  (the emulator enters and leaves exceptions by writing xPSR); QEMU then
  recognizes EXC_RETURN only in handler mode, as on the M4.
- The guest TLB index folds in higher address bits (64-bit x86 and AArch64
  hosts, C and JIT): with the page number alone, the first pages of flash,
  SRAM and the peripherals shared entries, so alternating accesses (literal
  pool, stack, MMIO) evicted each other and all took the slow path.
- `find_memory_mapping`, asked on every slow-path access (all MMIO), caches
  its answer per 1 KiB page until the memory map changes.
- Aligned MMIO reads and writes without memory hooks go straight to the
  device (`io_readx`/`io_writex`) instead of first looking up the region and
  scanning the empty hook lists, and for Unicorn's MMIO regions straight to
  the callback (`uc_its_mmio_read/write`) instead of through QEMU's memory
  API layers.
- The guest page size is a compile-time constant (1 KiB, as before;
  `TARGET_PAGE_BITS_FIXED` in `target/arm/cpu-param.h`), not loaded through
  a pointer chain in every TB lookup and TLB access. Setting another page
  size with `UC_CTL_UC_PAGE_SIZE` aborts.
- Write tracking for the emulator's idle detection: `uc_its_track_writes`
  registers a callback for the first store to each RAM page (QEMU's
  "not dirty" slow path, which Unicorn takes once per TLB entry anyway), and
  `uc_its_rearm_writes` re-arms it for the TLB of the current MMU index.
- `uc_its_core_regs` reads R0..R15 and xPSR in one call (idle detection at
  every loop head), without `uc_reg_read_batch`'s dispatch per register;
  `uc_its_set_core_regs` sets R0..R15 and NZCV after the emulator ran a
  polling loop in Unicorn's place.
- WFE and YIELD (QEMU's EXCP_YIELD) stop emulation as an invalid
  instruction, like an undefined one (EXCP_UDEF); `uc_its_stop_exception`
  tells which of the two it was.
- Direct jumps (`goto_tb`) to any page, and to blocks that span two pages:
  QEMU keeps them inside the block's page because an MMU could remap the
  other page; the M-profile guest has no MMU and the memory map never
  changes while code runs (`use_goto_tb`, `tb_find`). Fewer lookups of
  the next block (`helper_lookup_tb_ptr`) in busy code.
- `cpu_get_tb_cpu_state` is inline (`target/arm/cpu.h`): every lookup of the
  next translation block after an indirect branch (function returns) calls it.
- Blocks account for their cycles themselves (`uc_its_account`,
  `its_gen_account` in `accel/tcg/translator.c`): code at the start of each
  translated block adds its cycles to the emulator's clock and records how
  it ends, and calls the block hook only at the end of a slice, at the idle
  detector's loop heads and for blocks the emulator keeps for the hook
  (asked once the block is translated). The hook cost 60–70 % of the time
  of busy code on an arm64 host. The PC is written at the start of a block
  for the hook only.
- Indirect jumps (function returns, calls through pointers, table branches)
  look their target up inline (`its_gen_goto_ptr` in `target/arm/translate.c`):
  a table per CPU (`its_jc`, 256 entries) holds the translated code for
  (jump site, pc), filled by `helper_its_lookup_tb_ptr` and emptied whenever
  a block is invalidated. QEMU's `lookup_tb_ptr` recomputes the CPU state
  the next block is translated for on every jump (about 80 host
  instructions); at these jumps only the pc and the Thumb bit are dynamic
  (`DISAS_JUMP`), the rest is the same each time the jump runs, as for the
  direct jumps QEMU chains.
- On Apple Silicon the JIT area stays executable between translated blocks
  (`cpu_tb_exec`): Unicorn made it writable after every run of translated
  code and executable before the next (`pthread_jit_write_protect_np`
  twice). What writes to the area switches by itself, and `uc_emu_start`
  restores the thread's state. Reading the protection state
  (`read_sprr_perm`, in every API call such as `uc_reg_read`) no longer
  starts with an `isb`, and the consistency check `assert_executable` is
  for `CONFIG_DEBUG_TCG` only. Together a third of the time of firmware
  that leaves the translated code often (I2cLm75: an interrupt pending
  while masked, slices of 1000 cycles).
- Only the Arm guest: the other targets' sources are removed (`uc.c` still
  includes their `unicorn.h`).

Build: `build.rs` watches the crate's own sources (the original watches
paths of the Unicorn repository, which do not exist in the crate, so the
build script and everything above it were rebuilt on every `cargo build`).

To update: unpack the new crate here, remove `qemu/target/*` except `arm`
(keeping each `unicorn.h`) and apply the patch.
