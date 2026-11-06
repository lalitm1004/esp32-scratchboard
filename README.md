# ESP32-S3 joystick tracker

A PS2 analog joystick wired to an ESP32-S3, showing a **circle with a dot** on
an **ILI9341 (240×320) TFT** — the dot tracks the stick, and turns red when you
press the stick button. Rust + `esp-hal` + Embassy (`esp-rtos`) + `mipidsi`.

## Wiring

Display (ILI9341):

| ILI9341 pin | ESP32-S3 GPIO |
|-------------|---------------|
| SCL / SCK   | 12            |
| SDA / MOSI  | 11            |
| CS          | 10            |
| DC / RS     | 9             |
| RST         | 14            |
| BLK (LED)   | 2             |
| VCC / GND   | 3.3V / GND    |

Joystick (PS2 module):

| Joystick pin | ESP32-S3 GPIO | Notes |
|--------------|---------------|-------|
| VCC (+5V)    | **3.3V**      | use 3.3V, not 5V (ADC is 3.3V max) |
| GND          | **GND**       | |
| VRx (X)      | **4**         | ADC1_CH3 |
| VRy (Y)      | **5**         | ADC1_CH4 |
| SW           | **6**         | active-low button, internal pull-up |

> All pins are `const`s at the top of `src/main.rs`.

## Usage

```sh
nix develop
just build     # compile only
just flash     # flash the built binary
just monitor   # open serial
```

The stick is auto-centered at boot (assume it's released when you power on).
If an axis is inverted, flip the sign in `map_axis` in `src/main.rs`.

## Tuning knobs (`src/main.rs`)

- `RING_RADIUS` — size of the circle
- `DOT_RADIUS` — size of the dot
- `DEADZONE` — joystick jitter deadzone (raw ADC units)
