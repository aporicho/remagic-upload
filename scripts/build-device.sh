#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
TARGET=aarch64-unknown-linux-gnu
SDK=${RM_SDK:?set RM_SDK to a current reMarkable Paper Pro family SDK}
ENV_FILE=$(find "$SDK" -maxdepth 1 -name 'environment-setup-*' -print -quit)
if [[ -z "$ENV_FILE" ]]; then
    echo "reMarkable SDK environment was not found under $SDK" >&2
    exit 1
fi

unset LD_LIBRARY_PATH
# shellcheck disable=SC1090
source "$ENV_FILE"
compiler_name=${CC%% *}
compiler=$(command -v "$compiler_name")
sysroot=${SDKTARGETSYSROOT:-${OECORE_TARGET_SYSROOT:-}}
if [[ -z "$compiler" || -z "$sysroot" || ! -d "$sysroot" ]]; then
    echo "reMarkable SDK compiler/sysroot contract is incomplete" >&2
    exit 1
fi

export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=$compiler
export CC_aarch64_unknown_linux_gnu=$compiler
export CFLAGS_aarch64_unknown_linux_gnu="-O2 -pipe --sysroot=$sysroot -march=armv8-a"
export RUSTFLAGS="-C target-cpu=generic -C link-arg=--sysroot=$sysroot -C link-arg=-march=armv8-a"

cd "$ROOT"
cargo build --locked --release --target "$TARGET"
binary=$ROOT/target/$TARGET/release/remagic-upload
file "$binary" | grep -q 'ELF 64-bit.*ARM aarch64' || {
    echo "file upload binary is not AArch64 ELF" >&2
    exit 1
}
while IFS= read -r library; do
    case "$library" in
        libc.so.*|libgcc_s.so.*|libdl.so.*|libm.so.*|libpthread.so.*|librt.so.*) ;;
        *) echo "unexpected device-specific dependency: $library" >&2; exit 1 ;;
    esac
done < <(readelf -d "$binary" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p')
echo "built universal ReMagic application: $binary"
