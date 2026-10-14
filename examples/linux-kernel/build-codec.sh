#!/bin/sh
set -eu
src=$1
obj=$2
kdir=$3
linker=$4
objcopy=$5

test -f "$kdir/scripts/target.json" || {
    echo 'gitfs requires an x86-64 kernel build tree with scripts/target.json (CONFIG_RUST=y).' >&2
    exit 1
}
mkdir -p "$obj/build"
cp "$kdir/scripts/target.json" "$obj/build/gitfs-target.json"
flags='-Cpanic=abort -Crelocation-model=static -Ccode-model=kernel -Cno-redzone=y -Cforce-frame-pointers=y -Cforce-unwind-tables=n -Ctarget-feature=-sse,-sse2,-sse3,-ssse3,-sse4.1,-sse4.2,-avx,-avx2'
flags="$flags --cfg aes_force_soft --cfg polyval_force_soft"
if rustc -C help | rg -q 'jump-tables='; then
    flags="$flags -Cjump-tables=n"
else
    flags="$flags -Zno-jump-tables"
fi
json_flag=''
if RUSTC_BOOTSTRAP=1 cargo -Z help | rg -q 'json-target-spec'; then
    json_flag='-Zjson-target-spec'
fi
if rg -q '^CONFIG_MITIGATION_RETHUNK=y' "$kdir/.config"; then
    flags="$flags -Zfunction-return=thunk-extern"
fi
if rg -q '^CONFIG_X86_KERNEL_IBT=y' "$kdir/.config"; then
    flags="$flags -Zcf-protection=branch"
fi
# Rebuild core for the kernel ABI as well: host libcore is not kernel-safe.
# RUSTC_BOOTSTRAP enables the same unstable codegen controls Kbuild uses.
RUSTC_BOOTSTRAP=1 RUSTFLAGS="$flags" CARGO_PROFILE_RELEASE_OPT_LEVEL=3 cargo build \
    --manifest-path "$src/codec/Cargo.toml" --locked --release --features kernel \
    --target "$obj/build/gitfs-target.json" --target-dir "$obj/build/target" \
    -Z build-std=core,compiler_builtins -Z build-std-features= \
    -Z unstable-options $json_flag
"$linker" -r --gc-sections \
    -u gitfs_decode_loose -u gitfs_commit_tree -u gitfs_tree_next \
    -u gitfs_tls_size -u gitfs_tls_open -u gitfs_tls_read -u gitfs_tls_write \
    -u gitfs_smart_head -u gitfs_smart_pack -u gitfs_pack_import -u gitfs_pack_find \
    -o "$obj/codec.o" "$obj/build/target/gitfs-target/release/libgit_zero_kernel_codec.a"
"$objcopy" --remove-section=.llvmbc --remove-section=.llvmcmd "$obj/codec.o"
