# Source this file to use the Arm toolchain installed by VS Code / vcpkg
# (CMSIS-Toolbox + Arm Compiler 6) from a shell:  . tools/arm-env.sh
_artifacts=$(ls -d "$HOME"/.vcpkg/artifacts/*/ 2>/dev/null | head -n1)
_latest() { ls -d "$_artifacts$1"/*/ 2>/dev/null | sort -V | tail -n1; }

_toolbox=$(_latest tools.open.cmsis.pack.cmsis.toolbox)
_ac6=$(_latest compilers.arm.armclang)
_cmake=$(_latest tools.kitware.cmake)
_ninja=$(_latest tools.ninja.build.ninja)

if [ -z "$_toolbox" ] || [ -z "$_ac6" ]; then
    echo "arm-env: CMSIS-Toolbox or Arm Compiler 6 not found under ~/.vcpkg/artifacts" >&2
else
    _ac6_ver=$(basename "$_ac6")
    export CMSIS_PACK_ROOT="${CMSIS_PACK_ROOT:-$HOME/.cache/arm/packs}"
    export CMSIS_COMPILER_ROOT="${_toolbox}etc"
    export "AC6_TOOLCHAIN_$(echo "$_ac6_ver" | tr . _)=${_ac6}bin"
    export PATH="${_toolbox}bin:${_ac6}bin:${_cmake}bin:${_ninja}:$PATH"
fi
unset _artifacts _toolbox _ac6 _cmake _ninja _ac6_ver
unset -f _latest
