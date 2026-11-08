#!/usr/bin/env bash
# CI, all of it, on this machine: nothing runs on GitHub. The gate runs before every push: no crates, clippy, every test
# with the end-to-end harnesses and drop-ins, the embedded targets' tripwires and the two boards under QEMU. Each mode checks
# a commit, not the working tree, in a worktree of its own at target/ci, so another session's unsaved edits are not what passes.
#   ./ci.sh [rev]            the gate on rev, HEAD by default
#   ./ci.sh coverage [rev]   the uncovered lines, listed, not excused (cargo-llvm-cov 0.6.16, the one crate tool, in SECURITY.md)
#   ./ci.sh release <tag>    the tag's signature checked against the pinned key, the gate and the long runs green, then
#                            the library built twice, byte-identical, and summed, then every package from it. The long runs are the seeded mutators
#                            and soaks at their long counts and constant time measured: fixed seeds on unchanged code
#                            give the same answer, so they run once, before a release
# Needs what the drop-ins build with: Node 22, a JDK 25, .NET 8, Swift, Go, PHP with composer and grpc_php_plugin, a C++ compiler, protobuf found by pkg-config,
# qemu-system-arm and qemu-system-riscv32, the Android NDK; a release also conan and vcpkg.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
case ${1:-} in coverage|release) mode=$1; shift ;; *) mode=gate ;; esac
[ $mode != release ] || [ -n "${1:-}" ] || { echo "./ci.sh release <tag>"; exit 1; }
rev=${1:-HEAD} sha=$(git rev-parse --verify "${1:-HEAD}^{commit}")
tree=$root/target/ci
mkdir -p "$root/target"
until mkdir "$tree.lock" 2>/dev/null; do echo "waiting for the run holding $tree.lock"; sleep 10; done
trap 'rmdir "$tree.lock"' EXIT
[ -e "$tree/.git" ] || git worktree add -q --detach "$tree" "$sha"
git -C "$tree" checkout -q --detach "$sha"
cd "$tree"
echo "== $mode on $(git log -1 --format='%h %s')"
# timeout(1), which macOS lacks, killing the command's whole process group, so a hung drop-in under cargo dies with it
limit() { perl -e '$t = shift; $p = fork // die; unless ($p) { setpgrp; exec @ARGV or die } $SIG{INT} = sub { kill INT => -$p };
  $SIG{ALRM} = sub { kill KILL => -$p; die "over ${t}s, killed: @ARGV\n" }; alarm $t; waitpid $p, 0; exit($? >> 8 || $? & 127)' "$@"; }
tools="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/bin"

gate() {
  echo "== no crates, in the workspace, the tripwire's or qemu's"
  for m in Cargo.toml tripwire/Cargo.toml qemu/Cargo.toml; do
    t=$(cargo tree --locked --manifest-path "$m" --workspace --all-features --edges normal,build,dev --prefix none --format '{p}')
    test -z "$(echo "$t" | grep -v -e '^mudrpc-' -e '^$')" || { echo "$t"; exit 1; }
  done
  echo "== clippy and every test"
  cargo clippy --locked --workspace --all-targets -- -D warnings
  limit 1200 cargo test --locked --workspace --no-fail-fast -- --include-ignored   # minutes when well; a stall fails, never waits
  MUDRPC_INTERNET=listen-on-public-addresses cargo clippy --locked -p mudrpc-adapters --features internet --all-targets -- -D warnings
  MUDRPC_INTERNET=listen-on-public-addresses limit 600 cargo test --locked -p mudrpc-adapters --features internet --test udp   # the build that may go public, which no release is

  echo "== the embedded targets"
  rustup component add llvm-tools   # llvm-size, llvm-nm and llvm-objdump
  for t in aarch64-unknown-linux-gnu wasm32-wasip1 wasm32-unknown-unknown; do cargo build --locked --release -p mudrpc-core --target $t; done
  for t in aarch64-apple-ios aarch64-apple-ios-sim aarch64-linux-android; do cargo rustc --locked --release -p mudrpc-c --target $t --crate-type staticlib; done   # the phones' libraries, which their SDKs link; Android's JNI test links its own
  # A tripwire against growth, not a footprint claim: llvm-size's text column (.text, .rodata, unwind tables) of the
  # tripwire binary, core, auth, gcm and the ABI without std linked as a product's release build links them. 2026-10-24,
  # rustc 1.91.1, with the ABI's six shapes: 150 782 thumbv7em, 139 118 thumbv6m, 151 394 thumbv8m,
  # 186 682 riscv32imc (before it, 89 242, 83 058, 89 854, 113 074); one limit, ten percent over the largest. aarch64
  # and wasm are not measured: core has no per-target code, so their growth is this growth, and the rlib measured before
  # was mostly metadata (450 972 bytes once, tripped by docs). 2026-10-25, the tripwire exercising the Signed<Gcm> peer
  # the ABI already shipped: 162 238, 155 338, 162 834, 202 270; the limit follows the rule. Then the strangers' cap and
  # the ABI's overlap checks: 162 794, 159 058, 163 510, 204 982. Then 145 514, 157 782, 146 114, 163 470, `Signed::new` taking a budget of 0 as 1 alone 17 KB of it on thumbv7em.
  # Conditional branches in the object code where the GCM sealer touches secrets, against today's count, since a reading
  # found LLVM turning GHASH's masks into branches on Thumb-1, where the host's timing test cannot see. `ghash` and
  # its `mul` have no symbol on any of the four targets, inlined into `mudrpc_gcm::tag`, so the count is tag's whole
  # body, each branch on a public length (the blocks of AD and ciphertext, the multiply's 128 rounds); the S-box keeps
  # its own, `aes::sbox`, `aes::encrypt` holds the rounds around it and `aes::schedule` the key expansion, branching
  # only on the round's parity. The tag compares are not counted: `Gcm::open`'s is inlined into the peer's `deliver`
  # and `Key::open`'s sits among that function's public branches; each is a fold through `black_box` and one branch.
  # Counted between each symbol's address and its end: Thumb's `b<cond>`, `cbz`, `cbnz`, `it`, and `tbb`, `tbh`, the
  # table branch a `match` on a secret would make; RISC-V's `beq`, `bne`, `blt`, `bge`, `bltu`, `bgeu` and the compressed
  # `c.beqz`, `c.bnez`, printed without aliases (its jump tables are a `jalr` like any return, not counted).
  # 2026-10-22, rustc 1.91.1: tag 12 thumbv7em, 13 thumbv6m, 12 thumbv8m, 9 riscv32imc; schedule 7, 8, 7, 3; sbox 0
  # and encrypt 3 on all four. No margin: the toolchain is pinned, one `if` planted on a secret bit adds one branch
  # on thumbv6m and riscv32imc and two on the others, and GHASH without its `black_box`es counts 32, 46, 32 and 9,
  # RISC-V where LLVM kept the masks.
  for t in thumbv7em-none-eabihf thumbv6m-none-eabi thumbv8m.main-none-eabihf riscv32imc-unknown-none-elf; do
    cargo build --manifest-path tripwire/Cargo.toml --profile tripwire --target $t   # fails to link if any panic survives in core, auth, gcm or the ABI
    bin=tripwire/target/$t/tripwire/tripwire
    size=$("$tools/llvm-size" $bin | awk 'NR == 2 { print $1 }')
    echo "core, auth, gcm and the ABI as code for $t: $size bytes"
    test "$size" -lt 222500
    cargo build --locked --release -p mudrpc-c --no-default-features --target $t
    a=$("$tools/llvm-size" target/$t/release/libmudrpc.a | awk '$6 ~ /^mudrpc/ { n += $1 } END { print n }')
    echo "the ABI, core, auth and gcm in libmudrpc.a for $t, as a firmware links it: $a bytes of text before its link keeps what it calls; not gated"
    m='^(b(eq|ne|cs|hs|cc|lo|mi|pl|vs|vc|hi|ls|ge|lt|gt|le)(\.w|\.n)?|cbn?z|it[te]{0,3}|tbb|tbh)$' flags='' tag=12 ks=7
    case $t in
      thumbv6m*) tag=13 ks=8 ;;
      riscv*) m='^(c\.)?b(eq|ne|lt|ge|ltu|geu|eqz|nez)$' flags='-M no-aliases' tag=9 ks=3 ;;
    esac
    for f in "tag $tag" "aes::sbox 0" "aes::encrypt 3" "aes::schedule $ks"; do
      set -- $f
      read -r at s <<< "$("$tools/llvm-nm" -S -C "$bin" | awk -v f="mudrpc_gcm::$1::" 'index($0, f) { print $1, $2 }')"
      test -n "$s" || { echo "mudrpc_gcm::$1 has no symbol: inlined, so count inside the function that holds it"; exit 1; }
      n=$("$tools/llvm-objdump" -d --no-show-raw-insn $flags --start-address=0x$at --stop-address=$((0x$at + 0x$s)) "$bin" | awk -F'\t' 'NF > 2 { print $2 }' | grep -Ec "$m" || true)
      echo "conditional branches in mudrpc_gcm::$1 on $t: $n, at most $2"
      test "$n" -le "$2"
    done
  done

  # The in-process checks and the scrub probes on each core's instruction set, Cortex-M through semihosting and RISC-V
  # through virt's UART; instructions counted on the M4 alone, as virt has no SysTick; cycles stay the rig's.
  echo "== the two boards under QEMU"
  m4='qemu-system-arm -M mps2-an386 -cpu cortex-m4 -semihosting-config enable=on,target=native'
  (cd qemu   # cargo reads the linker script's flag from .cargo/config.toml where it runs
    for q in "thumbv7em-none-eabihf $m4" "riscv32imc-unknown-none-elf qemu-system-riscv32 -M virt -bios none"; do
      set -- $q
      cargo build --profile tripwire --target $1
      limit 120 "${@:2}" -nographic -kernel target/$1/tripwire/qemu 2>&1 | tee ../target/qemu.txt   # semihosting writes to stderr, the UART to stdout
      test "$(tail -n 1 ../target/qemu.txt)" = "ok 13 scrub seal"
    done
    cargo build --profile tripwire --target thumbv7em-none-eabihf --features icount   # the counts SPEC.md states, verify at most 5 500 000
    limit 120 $m4 -icount shift=0 -nographic -kernel target/thumbv7em-none-eabihf/tripwire/qemu 2>&1)
}

long() {
  export MUDRPC_FUZZ=1000000
  cargo test --release -p mudrpc-core -p mudrpc-auth -p mudrpc-gcm -p mudrpc-adapters -p mudrpc-c --test harden --test signed --test fuzz --test property --test wire --test sim --test abi   # the soak at a billion wires, the churn at a hundred million numbers
  cargo test --release -p mudrpc-adapters --test e2e -- --ignored long_   # the campaign's long runs: its largest logs held to the code, a thousand drones churning, a minute of a 1024-fleet's telemetry
  cargo test --release -p mudrpc-auth --test ct -- --ignored --nocapture   # constant time, measured: the tag compare, sign, the AES block and GHASH; the t statistics are in the log
  cargo test --release -p mudrpc-core -p mudrpc-auth -p mudrpc-gcm --test scrub   # the scratch dies: a stack probe after sign, a call signed or not and a seal and open under either cipher, release shape only
}

coverage() {
  cargo llvm-cov --version | grep -q ' 0\.6\.16' || { echo "cargo install cargo-llvm-cov --version 0.6.16 --locked, then rustup component add llvm-tools-preview"; exit 1; }
  cargo llvm-cov --workspace --text --output-path target/coverage.txt && cargo llvm-cov report --summary-only
  echo "the uncovered lines: $tree/target/coverage.txt"
}

release() {
  key=E68AB65A32F7C34F7A860AF7B6995AF47BE0D3B4   # the primary key of .github/release-key.asc, "mudRPC release key"
  export GNUPGHOME=$(mktemp -d)   # a keyring holding that file alone, so no other key on this machine can sign a release
  gpg -q --import .github/release-key.asc
  git verify-tag "$rev"   # fails on an unsigned tag, or one no key in the file made
  test "$(git verify-tag --raw "$rev" 2>&1 | awk '$2 == "VALIDSIG" { print $NF }')" = $key   # the signer's primary key is the pinned one, not one added to the file
  gate; long   # no release from a commit the gate and the long runs have not passed
  out=$root/target/dist/$rev ext=$([ "$(uname)" = Darwin ] && echo dylib || echo so)
  rm -rf "$out"
  id=$([ $ext = so ] || echo -C link-arg=-Wl,-install_name,@rpath/libmudrpc.dylib)   # not the build directory's path, which a host has not got and the two builds do not share
  build() { export CARGO_TARGET_DIR=$out/$1-target; cargo rustc --locked --release -p mudrpc-c --crate-type staticlib,cdylib -- $id; cargo build --locked --release -p mudrpc-c --target wasm32-unknown-unknown; mkdir -p "$out/$1"; cp $CARGO_TARGET_DIR/release/libmudrpc.{a,$ext} $CARGO_TARGET_DIR/wasm32-unknown-unknown/release/mudrpc.wasm bindings/c/mudrpc.h "$out/$1"/; }
  build out1; build out2   # every dependency built twice; not the .d, which names its target directory, nor the .rlib, which no C host links
  diff -r "$out/out1" "$out/out2"
  echo "reproducible"
  (cd "$out/out1" && shasum -a 256 libmudrpc.* mudrpc.* | tee SHA256SUMS)   # named, so the sums never list themselves
  packages/build.sh "${rev#v}" "$out/out1" "$out/packages"   # every package manager's artefact from that library, each installed into a fresh project
  echo "the release: $out/out1, the packages: $out/packages"
}

$mode
echo "== $mode green on $(git rev-parse --short HEAD)"
