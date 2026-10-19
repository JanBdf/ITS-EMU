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
- Only the Arm guest: the other targets' sources are removed (`uc.c` still
  includes their `unicorn.h`).

To update: unpack the new crate here, remove `qemu/target/*` except `arm`
(keeping each `unicorn.h`) and apply the patch.
