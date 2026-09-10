#!/bin/sh
# Cross-check the crates that do not build llama.cpp for macOS from Linux
# (m38). A fake `clang` writes an empty object for every compile so the C
# build scripts (libsqlite3-sys, ring, aws-lc-sys) succeed; nothing links.
# Needs `rustup target add x86_64-apple-darwin`.
set -eu
shim="${TMPDIR:-/tmp}/chronicle-mac-shim"
mkdir -p "$shim"
cat > "$shim/clang" <<'SH'
#!/bin/sh
out=""; prev=""
for a in "$@"; do
  case "$a" in -Fo*) out="${a#-Fo}";; esac
  if [ "$prev" = "-o" ]; then out="$a"; fi
  prev="$a"
done
case " $* " in *" -E "*|*" --version "*|*" -dumpversion "*) exec /usr/bin/clang "$@" ;; esac
if [ -n "$out" ]; then : > "$out"; fi
exit 0
SH
chmod +x "$shim/clang"
ln -sf clang "$shim/clang++"
export CC_x86_64_apple_darwin="$shim/clang" CXX_x86_64_apple_darwin="$shim/clang++"
command -v llvm-ar >/dev/null && export AR_x86_64_apple_darwin=llvm-ar
exec cargo check --target x86_64-apple-darwin \
  -p chronicle-capture -p chronicle-core -p chronicle-server -p chronicle-mcp "$@"
