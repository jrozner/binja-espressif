#!/usr/bin/env bash
# Fills DIR with what scripts/gen_rom_types.py parses, from the
# espressif/idf docker image of the ESP-IDF version the ROM type libraries
# are generated from (IDF_VERSION in gen_rom_types.py), or of VERSION, for
# gen_rom_types.py --older (which reads only the linker scripts and
# esp_idf_version.h):
#
#   DIR/components/      ESP-IDF's components: headers and linker scripts
#   DIR/newlib-xtensa/   the xtensa-esp-elf toolchain's newlib headers
#   DIR/newlib-riscv/    the riscv32-esp-elf toolchain's newlib headers
#   DIR/sdkconfig/<chip>/sdkconfig.h
#                        the default configuration of the hello_world example
#                        for each chip, with newlib as the C library: the
#                        ROMs' C library is newlib, but ESP-IDF builds with
#                        picolibc by default since v6.0, and its platform
#                        headers declare newlib's types (struct _reent) only
#                        for newlib
#
# Usage: scripts/fetch_rom_type_headers.sh DIR [VERSION]
set -euo pipefail
IMAGE=espressif/idf:${2:-v6.1}
CHIPS="esp32 esp32s2 esp32s3 esp32c2 esp32c3 esp32c6 esp32h2 esp32c5"

[ $# -eq 1 ] || [ $# -eq 2 ] || { echo "usage: $0 DIR [VERSION]" >&2; exit 1; }
mkdir -p "$1"
out=$(realpath "$1")

docker run --rm -i -v "$out":/out -e CHIPS="$CHIPS" "$IMAGE" bash -s <<'EOF'
set -euo pipefail
. /opt/esp/idf/export.sh >/dev/null
cd /opt/esp/idf
find components \( -name '*.h' -o -name '*.ld' \) -print0 | tar -c --null -T - -f - | tar -x -C /out
for arch in xtensa riscv32; do
    include=$(echo /opt/esp/tools/$arch-esp-elf/*/$arch-esp-elf/$arch-esp-elf/include)
    mkdir -p /out/newlib-${arch%32}
    cp -r "$include/." /out/newlib-${arch%32}/
done
for chip in $CHIPS; do
    rm -rf /tmp/hello && cp -r examples/get-started/hello_world /tmp/hello
    echo CONFIG_LIBC_NEWLIB=y >> /tmp/hello/sdkconfig.defaults  # unknown before v6.0, and ignored
    (cd /tmp/hello && idf.py --preview set-target "$chip" >/dev/null)
    mkdir -p /out/sdkconfig/$chip
    if grep -q '^#define CONFIG_LIBC_PICOLIBC 1$' /tmp/hello/build/config/sdkconfig.h; then
        echo "$chip: the configuration is for picolibc, not newlib" >&2
        exit 1
    fi
    cp /tmp/hello/build/config/sdkconfig.h /out/sdkconfig/$chip/
done
chown -R "$(stat -c %u:%g /out)" /out
EOF
echo "ESP-IDF headers for $CHIPS are in $out"
