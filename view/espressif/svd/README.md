# SVD files

The CMSIS-SVD files that `build.rs` compresses into the plugin, one per
supported chip. Espressif's are as Espressif published them, unmodified:

- ESP32, ESP32-S2, ESP32-S3, ESP32-C2, ESP32-C3, ESP32-C6 and ESP32-H2: from
  [espressif/svd](https://github.com/espressif/svd) (no longer maintained),
  directory `svd/`.
- ESP32-C5: from [espressif/esp-bist](https://github.com/espressif/esp-bist),
  `src/soc/esp32c5/esp32c5.svd`, since espressif/svd has no file for it.

Known upstream error: the ESP32-C5 file puts `LP_APM0` at `LP_APM`'s address
(0x600B3800; ESP-IDF's `reg_base.h` has it at 0x60099800), so that block is
named `LP_APM0`. The other base addresses that `reg_base.h` (ESP-IDF v5.4.1)
lists match it.

The files are Copyright Espressif Systems and licensed under the Apache
License, Version 2.0 ([LICENSE](LICENSE), from espressif/svd); each carries
the license text in its `<licenseText>`. (The rest of esp-bist is LGPL-3.0;
only the SVD file is used.)

## ESP8266

Espressif never published an SVD file for the ESP8266. `esp8266.svd` is
generated, with [svdtools](https://github.com/rust-embedded/svdtools) 0.5.0,
from [esp-rs/esp-pacs](https://github.com/esp-rs/esp-pacs) at the last commit
before its ESP8266 package was removed (c01397f): its `esp8266.base.svd`
(made by esp-rs' idf2svd from the ESP8266 SDK's register headers) with
esp-pacs' own svdtools patches, and then this directory's
[esp8266.yaml](esp8266.yaml). The esp-pacs files are not kept here;
`scripts/fetch_svds.sh` downloads them (the `esp-pacs/` lines of SOURCES) to
a temporary directory, makes the file again and checks it.

esp8266.yaml checks esp-pacs' file against ESP8266_RTOS_SDK v3.4's register
headers (`components/esp8266/include/esp8266/`) and makes it fit Binary
Ninja's SVD loader, which maps every address block as a segment typed as one
structure and adds a register's fields to it as bit fields, a later one
overwriting an earlier one it overlaps:

- the address blocks end at the last register the SDK defines, before the
  next peripheral; esp-pacs' sizes made 8 blocks overlap the next;
- the whole-register placeholder field `Register` that idf2svd gives many
  registers is gone. It overlapped the real fields of 18 registers, and a
  register whose only field it was is now a plain integer;
- where esp-pacs adds a second view of the same bits, one is kept: SPI
  `SPI_ADDR` is one 32-bit `addr` (as in the SDK's `spi_dev_t`, not
  address/size), `SPI_SLAVE` has the five interrupt enable bits rather than
  `spi_int_en`, `FRC1_CTRL`/`FRC2_CTRL` have their bit fields rather than a
  whole byte, and `UART_FIFO` has one byte, `rw_byte`;
- `I2SCONF_CHAN` (I2S +0x2C) and the RTC registers the SDK names that
  esp-pacs lacks are added: `RTC_SLP_VAL` (+0x04), `RTC_STATE2` (+0x18),
  `RTC_SLP_CNT_VAL` (+0x1C) and `RTC_STORE1` to `RTC_STORE3` (+0x34 to
  +0x3C, the SDK's `RTC_SCRATCH1` to 3);
- esp-pacs' `RTCCNTL` at 0x60000D00, the internal I2C master of the analog
  blocks (the PLL), not the RTC (that is `RTC`), is renamed `I2C_ANA`;
- the device gets a description and a `<licenseText>` naming its sources,
  and the CPU loses its FPU.

That leaves 15 peripherals, 233 registers and 840 fields (esp-pacs' patched
file has 15, 226 and 891; its base file 13, 204 and 671), with no address
block or field overlapping another. Covered: DPORT, EFUSE, the RNG register,
UART0/1, SPI0/1, GPIO, IO_MUX, TIMER (FRC1/FRC2), RTC, WDT, SLC, I2S and the
analog I2C master's one register. Not covered: the Wi-Fi MAC/baseband block
(WDEV, 0x3FF20000 on, apart from the RNG register at 0x3FF20E44), the SAR
ADC, and the analog blocks behind the I2C master.

esp-pacs is Copyright The esp-rs Project Developers and licensed under the MIT
License or the Apache License, Version 2.0, at your option
([LICENSE-esp-pacs-MIT](LICENSE-esp-pacs-MIT),
[LICENSE-esp-pacs-APACHE](LICENSE-esp-pacs-APACHE), from esp-pacs at the
same commit); `esp8266.svd`'s `<licenseText>` says so. esp8266.yaml is part
of this repository (MIT). The copy of the same SVD file in arduino-esp32 is
not used (that repository is LGPL).

## Updating

[SOURCES](SOURCES) pins each file to a commit and records its SHA-256 (for
`esp8266.svd`, of the generated file); `scripts/fetch_svds.sh` downloads
them again and checks them. For `esp8266.svd` it needs svdtools 0.5.0
(`cargo install svdtools --version 0.5.0 --locked`). After a change to
esp8266.yaml, run `scripts/fetch_svds.sh --update esp8266.svd` and record the
new SHA-256 it prints.
