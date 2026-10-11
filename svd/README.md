# Espressif SVD files

The CMSIS-SVD files that `build.rs` compresses into the plugin, one per
supported chip, as published by Espressif and unmodified:

- ESP32, ESP32-S2, ESP32-S3, ESP32-C2, ESP32-C3, ESP32-C6 and ESP32-H2: from
  [espressif/svd](https://github.com/espressif/svd) (no longer maintained),
  directory `svd/`.
- ESP32-C5: from [espressif/esp-bist](https://github.com/espressif/esp-bist),
  `src/soc/esp32c5/esp32c5.svd`, since espressif/svd has no file for it.

Known upstream error: the ESP32-C5 file puts `LP_APM0` at `LP_APM`'s address
(0x600B3800; ESP-IDF's `reg_base.h` has it at 0x60099800), so that block is
named `LP_APM0`. The other base addresses that `reg_base.h` (ESP-IDF v5.4.1)
lists match it.

[SOURCES](SOURCES) pins each file to a commit and records its SHA-256;
`scripts/fetch_svds.sh` downloads them again and checks them.

The files are Copyright Espressif Systems and licensed under the Apache
License, Version 2.0 ([LICENSE](LICENSE), from espressif/svd); each carries
the license text in its `<licenseText>`. (The rest of esp-bist is LGPL-3.0;
only the SVD file is used.)
