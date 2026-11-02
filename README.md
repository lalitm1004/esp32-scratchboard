# ESP32-S3 spinning donut

A shaded, rotating 3D torus rendered on an **ILI9341 (240×320) TFT** over SPI,
written in Rust with [`esp-hal`](https://github.com/esp-rs/esp-hal) +
[Embassy](https://embassy.dev) (`esp-rtos` executor) and
[`mipidsi`](https://github.com/almindor/mipidsi) for the display.

## Prerequisites

- NixOS with the provided `flake.nix` (installs `espup`, `espflash`, etc. and
  sets up the `esp` Rust toolchain).
- An ESP32-S3 board wired to an ILI9341 TFT.

## Wiring

| ILI9341 pin | ESP32-S3 GPIO |
|-------------|---------------|
| SCL / SCK   | 12            |
| SDA / MOSI  | 11            |
| CS          | 10            |
| DC / RS     | 9             |
| RST         | 14            |
| BLK (LED)   | 2             |
| VCC         | 3.3V          |
| GND         | GND           |

> The pins are constants near the top of `src/main.rs` if yours differ.

## Usage

Enter the dev shell (first run installs the toolchain — takes a few minutes):

```sh
nix develop
```

Then:

```sh
just build     # compile only (does NOT touch the device)
just flash     # flash the already-built binary
just monitor   # open the serial monitor
```

Building and flashing are intentionally separate steps — `just flash` never
rebuilds (it errors if you haven't run `just build` yet). For a one-shot
"flash then watch serial", use `just flash-monitor`. FPS is logged over serial
once per second.

## Troubleshooting

- **Blank / white screen**: check `ColorOrder::Bgr` (ILI9341 is BGR) and that
  RST/BL wiring matches. Try lowering `SPI_FREQ_MHZ` to 20 or 10.
- **Garbled colors**: confirm the color order is `Bgr` in `src/main.rs`.
- **Linker not found**: make sure you're inside `nix develop` so the espup
  environment (and the `esp` toolchain) is sourced.
