# ESP32-S3 + ILI9341 TFT + Embassy (esp-hal) — Reference Notes

Everything learned building a `no_std` Rust project that renders a spinning 3D
donut on an ILI9341 (240×320) TFT, driven over SPI by an ESP32-S3 with the
Embassy async runtime. Written as a reusable reference: stack versions, the
hard-won API details, memory/linker gotchas, wiring, and troubleshooting.

> Versions and facts below are current as of **November 2026**.

---

## 1. The stack (exact versions)

`Cargo.toml`:

| Crate | Version | Features | Why |
|-------|---------|----------|-----|
| `esp-hal` | `~1.2.2` | `esp32s3`, `log-04`, `unstable` | Hardware abstraction layer (`unstable` needed for I2S) |
| `esp-rtos` | `0.4.0` | `embassy`, `esp32s3`, `log-04` | Embassy executor + timers for esp-hal 1.x |
| `esp-bootloader-esp-idf` | `0.6.0` | `esp32s3`, `log-04` | `esp_app_desc!()` + IDF bootloader glue |
| `esp-println` | `0.18.0` | `esp32s3`, `log-04` | `log` backend over UART/USB-JTAG |
| `esp-backtrace` | `0.20.0` | `esp32s3`, `panic-handler`, `println` | Panic handler + backtrace |
| `log` | `0.4.27` | — | `info!` etc. |
| `embassy-executor` | `0.10.0` | `log` | The async executor (used via esp-rtos) |
| `embassy-time` | `0.5.0` | `log` | `Instant`, `Duration`, `Timer` |
| `static_cell` | `2.1.1` | — | `&'static mut` without `alloc` |
| `critical-section` | `1.2.0` | — | Version unification (esp-hal needs it) |
| `mipidsi` | `0.10.0` | *(default)* | MIPI DCS display driver (ILI9341) |
| `embedded-graphics-core` | `0.4.1` | — | `Rgb565` type |
| `embedded-hal-bus` | `0.3.0` | — | `ExclusiveDevice` (SpiBus → SpiDevice) |
| `libm` | `0.2.11` | — | `sinf`/`cosf`/`log10f` in `no_std` |
| `microfft` | `0.6.0` | `default-features = false`, `size-512` | no_std FFT (spectrum visualizer) |

Release profile (`Cargo.toml`): `codegen-units = 1`, `lto = "fat"`, `opt-level = "s"`, `debug = 2`.

### Ecosystem context

- **`esp-hal` 1.2.2** is the current stable 1.x line (released 2026-09).
- **`esp-rtos` 0.4.0** is the *current* Embassy integration crate (provides
  `#[esp_rtos::main]` and `esp_rtos::start`). The older `esp-hal-embassy` 0.9.x
  still exists but `esp-generate` now emits `esp-rtos`.
- **The `esp-hal` git `main` branch is NOT 1.x** — it's the next-gen (2.0)
  code with different feature names (e.g. `esp32s31`) and a different API.
  Don't copy examples from `main`; use the `esp-hal-1.2.x` branch or the
  released docs on docs.rs.

---

## 2. Toolchain & NixOS setup

### flake.nix

- Uses `pkgs.buildFHSEnv` — **required** because the espup toolchain binaries
  (cargo/rustc/xtensa-gcc) are dynamically linked against FHS-style `/lib`
  paths, which NixOS doesn't provide by default.
- `targetPkgs` must include: `rustup espup espflash esp-generate
  probe-rs-tools just gcc pkg-config openssl zlib libxml2 git curl python3
  cmake ninja`.
  - **`just` is easy to forget** — add it or `just` won't exist in the shell.
- The `profile` script sets `ESP_HOME="$HOME/.local/share/esp-rust"`,
  `RUSTUP_HOME`, `CARGO_HOME`, and runs `espup install --targets esp32s3`
  on first run (sources `$ESP_HOME/export-esp-latest.sh`).

### Toolchain

- espup installs a toolchain named **`esp`** (path:
  `~/.local/share/esp-rust/rustup/toolchains/esp`).
- `rust-toolchain.toml` → `channel = "esp"`.
- `rust-src` is present at
  `~/.local/share/esp-rust/rustup/toolchains/esp/lib/rustlib/src/rust`
  (needed for `build-std`).
- The export file
  (`~/.local/share/esp-rust/export-esp-latest.sh`) adds the xtensa gcc to
  `PATH`; cargo's build script checks for `xtensa-esp32s3-elf-gcc`.

### `nix develop` is interactive-only (FHS)

`nix develop` drops you into the FHS bash **only in interactive/tty mode**.
`nix develop --command ...` or piping stdin does **not** enter the FHS
chroot — it runs a plain shell with the host PATH (cargo not found, or "could
not start dynamically linked executable").

To run a command non-interactively through the FHS env, allocate a pty:

```sh
printf 'cargo build --release\nexit\n' | script -qec 'nix develop' /dev/null
```

### Broken `.git` blocks `nix develop`

If the project dir has a `.git` that's incomplete (missing `HEAD`/`config`),
Nix's flake resolution fails with:

```
error: opening Git repository "...": could not find repository at '...'
```

Fix — the flake files must be tracked:

```sh
git init
git add -A            # at least flake.nix (and flake.lock)
```

(`git init` alone isn't enough; Nix needs `flake.nix` *tracked*, so `git add`
is required. A commit removes the "dirty tree" warning but isn't required to
build.)

---

## 3. Project scaffolding (the required files)

| File | Purpose |
|------|---------|
| `build.rs` | Emits `-Tlinkall.x`, checks the xtensa linker is on PATH, nice linker errors |
| `.cargo/config.toml` | target + espflash runner + `build-std` |
| `.cargo/esp-config.toml` | `ESP_LOG = "info"` |
| `rust-toolchain.toml` | `channel = "esp"` |

`.cargo/config.toml` essentials (note: **no `runner`** — build/flash are kept
separate):

```toml
[target.xtensa-esp32s3-none-elf]
# (deliberately no `runner` so `cargo run` never flashes)

[build]
rustflags = ["-C", "link-arg=-nostartfiles"]
target = "xtensa-esp32s3-none-elf"

[unstable]
build-std = ["core"]   # needs the esp toolchain's rust-src component
```

`build.rs` essentials:

```rust
fn main() {
    check_xtensa_linker_available();
    println!("cargo:rustc-link-arg=-Tlinkall.x");   // MUST be last linker script
}
```

### Feature names (exact)

Every ESP crate takes a chip feature `esp32s3`. The `log` 0.4 integration
feature is `log-04` (not `log`). esp-rtos takes `embassy` to enable the
executor. esp-backtrace takes `panic-handler` + `println`.

---

## 4. esp-hal 1.x API (the important parts)

### Bootstrapping Embassy

```rust
#[esp_rtos::main]                      // NOT #[esp_hal::main] (use esp-rtos)
async fn main(_spawner: embassy_executor::Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);  // time driver
    // ...
}
```

- `esp_backtrace as _;` provides the `#[panic_handler]` (don't define your own).
- `esp_bootloader_esp_idf::esp_app_desc!();` is required.

### GPIO output — THREE arguments

```rust
use esp_hal::gpio::{Level, Output, OutputConfig};

let pin = Output::new(peripherals.GPIO9, Level::Low, OutputConfig::default());
```

**`Output::new(pin, level, config)` takes 3 args** in esp-hal 1.x (the config
is `OutputConfig`, not the old 2-arg form). `Output` implements
`embedded_hal::digital::OutputPin`.

### GPIO from a number (for `const` pin tables)

Pins are distinct types (`GpioPin<N>`), so a `const u8` needs a mapping.
Cleanest primitive is `AnyPin::steal`:

```rust
use esp_hal::gpio::AnyPin;

fn gpio(n: u8) -> AnyPin<'static> {
    // Safe if each pin number is claimed exactly once.
    unsafe { AnyPin::steal(n) }   // panics if n isn't a real GPIO number
}
```

`AnyPin` implements `OutputPin` and `PeripheralOutput`, so it works with both
`Output::new` and SPI pin methods.

### SPI: `Spi` is a `SpiBus`, NOT a `SpiDevice`

```rust
use esp_hal::spi::{Mode, master::{Config as SpiConfig, Spi}};
use esp_hal::time::Rate;

let spi = Spi::new(
    peripherals.SPI2,
    SpiConfig::default()
        .with_frequency(Rate::from_mhz(40))
        .with_mode(Mode::_0),
).unwrap()
    .with_sck(sclk)
    .with_mosi(mosi);
```

- `Spi` only implements `embedded_hal::spi::SpiBus`.
- `.with_cs()` exists but is `unstable`-gated and **still** doesn't yield a
  `SpiDevice`.
- To get a `SpiDevice` (what mipidsi needs), wrap it:

```rust
use embedded_hal_bus::spi::ExclusiveDevice;

let cs = Output::new(peripherals.GPIO10, Level::High, OutputConfig::default());
let spi_dev = ExclusiveDevice::new_no_delay(spi, cs).unwrap();
// (new_no_delay is fine for mipidsi — it never uses in-transaction delays)
```

### Delay

```rust
let mut delay = esp_hal::delay::Delay::new();  // blocking, uses SYSTIMER
```

Fine to use alongside the Embassy timer (which uses TIMG0). Needed for
mipidsi's `init` (which wants `embedded_hal::delay::DelayNs`).

---

## 5. Display: mipidsi + ILI9341

### Interface (mipidsi's own, NOT display-interface-spi)

```rust
use mipidsi::{interface::SpiInterface, models::ILI9341Rgb565, Builder};

let mut buffer = [0u8; 4096];          // SPI batch buffer (bigger = fewer txns)
let di = SpiInterface::new(spi_dev, dc, &mut buffer);
//  ^ 3 args: (SpiDevice, OutputPin, &mut [u8])
```

mipidsi 0.10 ships its **own** `SpiInterface` (`mipidsi::interface::SpiInterface`)
that takes `(spi: SpiDevice, dc: OutputPin, buffer)`. Do **not** use the
separate `display-interface-spi` crate with mipidsi 0.10.

### Builder

```rust
let mut display = Builder::new(ILI9341Rgb565, di)
    .reset_pin(rst)                 // handles HW reset (low → delay → high)
    .color_order(ColorOrder::Bgr)   // REQUIRED: ILI9341 is BGR
    .orientation(Orientation::new()) // Deg0 = 240x320 portrait
    .init(&mut delay)               // &mut impl DelayNs
    .unwrap();
```

- `ILI9341Rgb565` is the model (Rgb565 color); `FRAMEBUFFER_SIZE` = 240×320, so
  no `.display_size()` call needed.
- **`ColorOrder::Bgr` is mandatory** — the default is `Rgb`, and getting it
  wrong swaps red/blue.
- `.reset_pin(rst)` needs the RST pin to start high (`Level::High` or it'll be
  driven high for you).

### Drawing / blit

```rust
use embedded_graphics_core::pixelcolor::{Rgb565, RgbColor};  // RgbColor needed for Rgb565::BLACK

display.clear(Rgb565::BLACK).unwrap();
display.set_pixels(0, 0, 239, 319, framebuffer.iter().copied()).unwrap();
```

- `Rgb565` is `embedded_graphics_core::pixelcolor::Rgb565`, 2 bytes.
- `Rgb565::BLACK` (and `RED` etc.) come from the **`RgbColor` trait** — it must
  be in scope or `Rgb565::BLACK` won't resolve.
- `set_pixels(sx, sy, ex, ey, colors)` streams a full frame efficiently
  (sets the address window once, then RAMWR). Coordinates are inclusive.

---

## 6. Memory & linker (ESP32-S3)

### Memory map (from the generated `memory.x`)

```
dram_seg   (RW)  0x3FC88000 .. 0x3FCDB700   ≈ 333.75 KiB   ← RWDATA (data/bss/stack)
dram2_seg  (RW)  0x3FCDB700 .. 0x3FCED710   ≈  71 KiB     ← NOT aliased to RWDATA by default
dcache_reclaimed    0x3FCF0000 ..           0 by default
```

`.data`, `.bss`, `.noinit`, and the **stack** all live in `RWDATA = dram_seg`
(~333.75 KiB). The stack sits at the top of `RWDATA` and grows down; its size
is whatever's left after `.data`/`.bss`/`.noinit`, and the linker asserts it's
**≥ 8192 bytes**.

### Error 1: "Main stack is smaller than 8192 bytes" (link time)

Cause: `.bss` is too big (e.g. two 150 KiB framebuffers = 300 KiB), leaving
< 8 KiB for the stack.

Fix: shrink static buffers. For the donut, switching the depth buffer from
`u16` (150 KiB) to `u8` (75 KiB) freed enough. Budget: color `[Rgb565; 240*320]`
= 150 KiB + depth `[u8; 240*320]` = 75 KiB → 225 KiB total, ~108 KiB headroom.

### Error 2: "stack pointer out of range" (runtime panic)

```
panicked at esp-hal-1.2.2/src/soc/mod.rs:282:5: stack pointer out of range:
sp=0x3fcb51e0, bottom=0x3fcc6db4, top=0x3fcdb700
```

Cause: **`StaticCell::init([Rgb565::BLACK; N])` constructs the whole 150 KiB
array as a temporary on the stack** before moving it into the static. The
84 KiB main stack overflows.

Fix: declare large buffers `static mut` with a zero-initializer — the linker
zero-fills `.bss` for free, no stack temporary:

```rust
static mut COLOR: [Rgb565; N] = [Rgb565::BLACK; N];
static mut DEPTH: [u8; N] = [0u8; N];

let color = unsafe { &mut *core::ptr::addr_of_mut!(COLOR) };
let depth = unsafe { &mut *core::ptr::addr_of_mut!(DEPTH) };
```

Sound because `render()` writes every byte (`.fill()`) before any read, and
it's a single-threaded program. `StaticCell::init` is fine for *small*
values (SPI buffer, a struct).

### Rule of thumb

- Large buffers (`> ~4 KiB`): `static mut` + zero-init, access via `addr_of_mut!`.
- Small statics: `static_cell::StaticCell` (safe).
- Keep `.bss` comfortably under ~325 KiB on ESP32-S3, or the boot stack gets
  squeezed below 8 KiB.

---

## 7. The donut renderer

The classic spinning torus, but drawn as a shaded 3D surface (not ASCII).

### Geometry

Torus (object space), parameterised by ring angle θ and tube angle φ:

```
px = (R1 + R2·cos φ)·cos θ      nx = cos φ·cos θ
py = (R1 + R2·cos φ)·sin θ      ny = cos φ·sin θ
pz = R2·sin φ                   nz = sin φ
```

`R1 = 1.0`, `R2 = 0.42`. Trig tables for the θ/φ grids are precomputed **once**
(they never change); per frame only `sin/cos` of the three rotation angles are
needed.

### Rotation

Rotate about X by `a`, then about Z by `b` (normals rotate the same way):

```
// Rx(a):
py1 = py·cosA - pz·sinA ; pz1 = py·sinA + pz·cosA
// Rz(b):
px2 = px·cosB - py1·sinB ; py2 = px·sinB + py1·cosB   // pz2 = pz1
```

### Perspective projection & depth

```
inv = ZOOM / (DIST + pz1)        // ZOOM=90 (focal px), DIST=3
sx  = cx + px2 · inv             // cx = W/2, cy = H/2
sy  = cy + py2 · inv
```

`inv` is monotonic with closeness, so it *is* the depth: larger = closer.
Scale it into a `u8` (`zval = (inv * 4.0) as u8`, `0` = empty) and keep only
`zval > depth[idx]`. 256 levels is plenty — the near/far tube separation is
~32 levels, so no z-fighting.

### Shading

Orbiting unit light `L = (0.6·cos c, 0.6·sin c, 0.8)` (`0.6² + 0.8² = 1`),
Lambert + ambient:

```
lum = 0.12 + 0.88 · max(0, nx2·Lx + ny2·Ly + nz1·Lz)
```

`shade(lum)` maps `[0,1]` → `Rgb565` along a warm gradient (dark maroon →
bright pink/white).

### Speed

`A_SPEED` / `B_SPEED` / `C_SPEED` are radians **per frame**. Frame rate is
SPI-bound (~25–30 fps at 40 MHz), so these set absolute spin rate. Past
~0.6 rad/frame the donut strobes/blurs.

---

## 8. Wiring (default pin map)

| ILI9341 pin | ESP32-S3 GPIO | Notes |
|-------------|---------------|-------|
| SCL / SCK   | **12** | FSPICLK |
| SDA / MOSI  | **11** | FSPID |
| CS          | **10** | FSPICS0 |
| DC / RS     | **9**  | data/command |
| RST         | **14** | reset |
| BLK / LED   | **2**  | backlight (drive high) |
| VCC         | 3.3V  | |
| GND         | GND   | |
| MISO        | *(unused)* | write-only, no readback |

These are the ESP32-S3 **FSPI (SPI2) default pins** for SCK/MOSI/CS, with
DC/RST/BL on free GPIOs. Change them via the `PIN_*` consts in `src/main.rs`.

### Audio (MAX98357A over I2S0)

| MAX98357A pin | ESP32-S3 GPIO | Notes |
|---------------|---------------|-------|
| BCLK          | **4**  | I2S bit clock |
| LRC / WS      | **5**  | word select (left/right clock) |
| DIN           | **6**  | I2S data out → amp input |
| VIN           | 3.3V or 5V | 5 V = louder (2.5–5.5 V range) |
| GND           | GND   | common ground with the ESP32 |
| SD            | float / VIN | active-low shutdown; floating enables (pull-up) |
| GAIN          | float | optional gain select |
| Speaker ±     | speaker | |

No MCLK needed — the MAX98357A generates its internal clock from BCLK. Three
signal pins total; GPIOs 4/5/6 don't clash with the display pins.

---

## 8b. I2S audio playback (esp-hal)

### Setup (TX, Philips/I2S standard)

```rust
use esp_hal::i2s::master::{I2s, DataFormat, Channels, TdmConfig};
use esp_hal::time::Rate;

let i2s = I2s::new(
    peripherals.I2S0,
    peripherals.DMA_CH0,               // GDMA channel on ESP32-S3
    TdmConfig::new_tdm_philips()
        .with_sample_rate(Rate::from_hz(22050))
        .with_data_format(DataFormat::Data16Channel16) // 16-bit samples
        .with_channels(Channels::MONO),                // mono
).unwrap();

let mut i2s_tx = i2s.i2s_tx
    .with_bclk(gpio(PIN_I2S_BCLK))
    .with_ws(gpio(PIN_I2S_WS))
    .with_dout(gpio(PIN_I2S_DIN))
    .build();
```

- I2S (like SPI) needs the **`unstable` feature** on `esp-hal`.
- `i2s.i2s_tx` is a `TxCreator`; `.with_bclk/.with_ws/.with_dout` take pins,
  `.build()` returns `I2sTx`.
- `Channels::MONO` still emits the standard 2-slot (L/R) frame, but only the L
  slot carries data — the MAX98357A plays it.

### Streaming with DMA

```rust
use esp_hal::dma_tx_buffer;

let mut dma_buf = dma_tx_buffer!(2048).unwrap();   // DmaTxBuf

// ... fill dma_buf, then:
let transfer = i2s_tx.write(dma_buf).unwrap();     // consumes i2s_tx + buf
// ... do other work while DMA transmits ...
let (_, tx, buf) = transfer.wait();                // returns i2s_tx + buf
i2s_tx = tx; dma_buf = buf;
```

- `I2sTx::write(buf: DmaTxBuf)` consumes both and returns `I2sTxDmaTransfer`;
  `.wait()` blocks until the DMA is done and hands both back.
- `DmaTxBuf::fill(&[u8])` copies + sets the transmitted length, or use
  `as_mut_slice()` (full capacity) + `set_length(n)`.
- This single-buffer pattern is gap-free as long as the FFT/draw work fits
  inside one chunk's duration (46 ms for 2048 bytes @ 22.05 kHz).

### WAV embedding

- Re-encode to small PCM first (16-bit mono is ideal):
  `ffmpeg -i in.wav -ac 1 -ar 22050 -sample_fmt s16 out.wav`
- `const WAV: &[u8] = include_bytes!("../out.wav");` — lands in `.rodata`
  (flash-mapped), read directly (no RAM copy).
- Parse the `fmt `/`data` chunks at runtime to get sample rate + PCM offset, so
  the I2S config follows the file automatically.

### FFT visualizer (microfft)

```rust
use microfft::real::rfft_512;   // input [f32; 512] -> [Complex32; 256]

let mut buf = [0f32; 512];
// fill buf with windowed samples (i16 -> f32 / 32768, Hann window)
let bins = rfft_512(&mut buf);  // in-place, aliases buf as complex
let mag = bins[k].norm_sqr();   // re^2 + im^2
```

- `f32::powf` is **not** available in `no_std` — use `libm::powf`/`expf`/`logf`.
- Log-space the FFT bins into bars (bass gets narrow bins, treble gets grouped)
  for a natural look.

---

## 8c. Analog input (ADC) — joystick

The ADC (like I2S) needs the **`unstable`** feature. ESP32-S3 has ADC1
(GPIO1–10) and ADC2 (GPIO11–20); prefer ADC1.

```rust
use esp_hal::analog::adc::{Adc, AdcConfig, Attenuation};
use nb::block; // add `nb = "1.1"` to Cargo.toml

let mut cfg = AdcConfig::new();
let mut vrx = cfg.enable_pin(peripherals.GPIO4, Attenuation::_11dB); // AdcPin
let mut vry = cfg.enable_pin(peripherals.GPIO5, Attenuation::_11dB);
let mut adc = Adc::new(peripherals.ADC1, cfg);

let x: u16 = block!(adc.read_oneshot(&mut vrx)).unwrap(); // 0..4095
```

Key points:

- `AdcConfig::enable_pin(pin, attenuation)` returns an `AdcPin<PIN, ADC>`. The
  pin must be a **typed** `GpioPin<N>` (an ADC1 channel) — `AnyPin` does **not**
  implement `AdcChannel`, so you can't use `AnyPin::steal` for ADC pins.
- `read_oneshot` returns `nb::Result<u16, ()>` (non-blocking; the first call
  starts a conversion and returns `WouldBlock`). Poll with `nb::block!(...)`.
- `Attenuation::_11dB` gives ~0–3.1 V range; the other attenuations are for
  smaller full-scale voltages. Use 11 dB for a 0–3.3 V signal.
- The result is 12-bit (0–4095).

### Wiring a PS2 analog joystick module

| Joystick pin | ESP32-S3 | Notes |
|--------------|----------|-------|
| VCC          | 3.3V     | **not 5 V** — ADC input is 3.3 V max |
| GND          | GND      | |
| VRx          | GPIO4    | ADC1_CH3 (X axis) |
| VRy          | GPIO5    | ADC1_CH4 (Y axis) |
| SW           | GPIO6    | active-low button → `Input::new(..., Pull::Up)`, read `is_low()` |

Auto-center the stick at boot (read once, assume released), then map
`(raw - center)` to a screen offset. A small deadzone removes jitter.

---

## 9. Build & flash (justfile)

Building and flashing are deliberately **separate** — `just flash` never
rebuilds, so it's fast and can't accidentally recompile.

```sh
nix develop
just build     # cargo build --release  (compile only, no device access)
just flash     # espflash flash <bin>   (flashes the existing binary)
just monitor   # espflash monitor       (open serial)
just flash-monitor   # flash + monitor (no rebuild)
just erase     # espflash erase-flash
```

- `just flash` guards on the binary existing (`target/.../release/esp32`) and
  tells you to `just build` first if it's missing.
- `.cargo/config.toml` deliberately has **no `runner`** — `cargo run` won't
  flash anything; use the `just` recipes instead.
- Binary lands at `target/xtensa-esp32s3-none-elf/release/esp32`.
- `espflash` auto-detects the serial port; pass `--port /dev/ttyUSB0`
  (or `/dev/ttyACM0`) to override.

---

## 10. Troubleshooting

| Symptom | Cause / fix |
|---------|-------------|
| Blank or white screen | Backlight/RST wiring; or SPI too fast — drop `SPI_FREQ_MHZ` to 20 or 10 |
| Red/blue swapped colors | Missing `ColorOrder::Bgr` |
| Link: "Main stack is smaller than 8192 bytes" | Too much `.bss` — shrink static buffers (§6) |
| Panic: "stack pointer out of range" | Large array built as a stack temp — use `static mut` zero-init (§6) |
| "could not find repository" on `nix develop` | Broken `.git` — `git init && git add -A` (§2) |
| `cargo`/`rustc` not found in `nix develop` | Not in FHS env — it's interactive-only; use the `script` pty trick (§2) |
| "Xtensa linker not found" | espup export file not sourced — be inside `nix develop` |
| No serial output | Check port (`--port`); `ESP_LOG` level in `.cargo/esp-config.toml` |
| No sound from speaker | Check BCLK/WS/DIN wiring; `SD` must not be held low (active-low shutdown) |
| Garbled/crackly audio | WAV must be 16-bit PCM mono; `Channels::MONO` + `DataFormat::Data16Channel16`; confirm sample rate matches the WAV header |
| `no method named powf found for f32` | `f32::powf` needs `std` — use `libm::powf`/`expf`/`logf` |

---

## 11. Quick gotchas checklist

- [ ] `#[esp_rtos::main]`, `esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0)`
- [ ] `Output::new(pin, level, OutputConfig::default())` — 3 args
- [ ] `Spi` is a `SpiBus` → wrap with `ExclusiveDevice::new_no_delay(spi, cs)`
- [ ] `use embedded_graphics_core::pixelcolor::RgbColor;` for `Rgb565::BLACK`
- [ ] ILI9341 → `.color_order(ColorOrder::Bgr)`
- [ ] mipidsi `SpiInterface::new(spi, dc, buffer)` (3 args, own interface)
- [ ] Large buffers → `static mut` zero-init, not `StaticCell::init`
- [ ] Depth buffer can be `u8` (256 levels) to save RAM
- [ ] `just` in `flake.nix` `targetPkgs`
- [ ] `rust-toolchain.toml` → `channel = "esp"`
- [ ] `.git` must have `flake.nix` tracked for `nix develop`
