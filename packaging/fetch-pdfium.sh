#!/usr/bin/env sh
# Download the prebuilt PDFium for one target and stage it for the wheel (DESIGN.md §9).
#
#   packaging/fetch-pdfium.sh <platform>
#
# <platform> is a bblanchon/pdfium-binaries name: mac-arm64, mac-x64, linux-x64,
# linux-arm64, linux-musl-x64 or linux-musl-arm64.
#
# maturin's `data = "packaging/data"` copies packaging/data/data/** into the wheel's
# `.data/data/` directory, which installers place under the environment prefix. The
# library therefore ends up at <prefix>/share/dir-shuffle/, next to <prefix>/bin/,
# which is where the binary looks (src/render/pdf.rs).
set -eu

# Must match the `pdfium_<N>` feature in Cargo.toml and PDFIUM_BUILD in PiContainerfile.
PDFIUM_BUILD="${PDFIUM_BUILD:-8125}"

platform="${1:?usage: $0 <platform>}"
here="$(cd "$(dirname "$0")" && pwd)"
dest="$here/data/data/share/dir-shuffle"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

curl -fsSL "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F${PDFIUM_BUILD}/pdfium-${platform}.tgz" \
  | tar xz -C "$tmp"

mkdir -p "$dest"
find "$dest" -mindepth 1 ! -name .keep -exec rm -rf {} +
case "$platform" in
  mac-*) cp "$tmp/lib/libpdfium.dylib" "$dest/" ;;
  *)     cp "$tmp/lib/libpdfium.so" "$dest/" ;;
esac
# PDFium is redistributed in the wheel: ship its license and third-party notices.
cp "$tmp/LICENSE" "$dest/PDFIUM-LICENSE"
if [ -d "$tmp/licenses" ]; then
  cp -R "$tmp/licenses" "$dest/pdfium-licenses"
fi
echo "staged PDFium chromium/${PDFIUM_BUILD} (${platform}) in $dest"
