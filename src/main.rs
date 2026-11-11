#![no_std]
#![no_main]

use core::ptr::addr_of_mut;

use embassy_time::Timer;
use embedded_graphics_core::pixelcolor::{Rgb565, RgbColor};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::dma_tx_buffer;
use esp_hal::gpio::{AnyPin, Level, Output, OutputConfig};
use esp_hal::i2s::master::{Channels, DataFormat, I2s, TdmConfig};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use log::info;
use mipidsi::options::{ColorOrder, Orientation};
use mipidsi::{interface::SpiInterface, models::ILI9341Rgb565, Builder};
use static_cell::StaticCell;

// =============================================================================
// Configuration
// =============================================================================

// --- Display (ILI9341 over SPI) ---
const PIN_SCLK: u8 = 12;
const PIN_MOSI: u8 = 11;
const PIN_CS: u8 = 10;
const PIN_DC: u8 = 9;
const PIN_RST: u8 = 14;
const PIN_BL: u8 = 2;

// --- Audio (MAX98357A over I2S) ---
const PIN_I2S_BCLK: u8 = 4; // bit clock
const PIN_I2S_WS: u8 = 5; // word select (LRC)
const PIN_I2S_DIN: u8 = 6; // data in

const SPI_FREQ_MHZ: u32 = 40;

const WIDTH: i32 = 240;
const HEIGHT: i32 = 320;
const N: usize = (WIDTH * HEIGHT) as usize;

// Embedded audio: 16-bit mono 22050 Hz PCM.
const WAV: &[u8] = include_bytes!("../tv-girl-mono.wav");

// Bytes per DMA transfer (4096 = 2048 samples ~= 64 ms at 32 kHz). Sized to
// give the once-per-second display blit comfortable headroom over the chunk.
const CHUNK: usize = 4096;

// Digital volume: right-shift each 16-bit sample. 0 = full scale, 1 = -6 dB,
// 2 = -12 dB. The MAX98357A's default (+9 dB) gain clips a full-scale track,
// so attenuate here unless you've lowered the amp's GAIN in hardware.
const VOLUME_SHIFT: u32 = 1;

fn gpio(n: u8) -> AnyPin<'static> {
    unsafe { AnyPin::steal(n) }
}

struct Wav {
    sample_rate: u32,
    data: &'static [u8],
}

/// Minimal WAV parser (16-bit PCM only).
fn parse_wav(bytes: &'static [u8]) -> Wav {
    assert_eq!(&bytes[0..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");

    let mut offset = 12usize;
    let mut sample_rate = 0u32;
    let mut channels = 0u16;
    let mut bits = 0u16;
    let mut data = &bytes[..];

    while offset + 8 <= bytes.len() {
        let id = &bytes[offset..offset + 4];
        let size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let body = offset + 8;
        match id {
            b"fmt " => {
                let format = u16::from_le_bytes(bytes[body..body + 2].try_into().unwrap());
                assert_eq!(format, 1, "only PCM WAV is supported");
                channels = u16::from_le_bytes(bytes[body + 2..body + 4].try_into().unwrap());
                sample_rate = u32::from_le_bytes(bytes[body + 4..body + 8].try_into().unwrap());
                bits = u16::from_le_bytes(bytes[body + 14..body + 16].try_into().unwrap());
            }
            b"data" => {
                data = &bytes[body..body + size];
                break;
            }
            _ => {}
        }
        offset = body + size + (size & 1);
    }

    assert_eq!(channels, 1, "expecting mono WAV");
    assert_eq!(bits, 16, "expecting 16-bit WAV");
    Wav { sample_rate, data }
}

// --- drawing helpers ---

fn plot(fb: &mut [Rgb565; N], x: i32, y: i32, color: Rgb565) {
    if x >= 0 && x < WIDTH && y >= 0 && y < HEIGHT {
        fb[y as usize * WIDTH as usize + x as usize] = color;
    }
}

fn fill_rect(fb: &mut [Rgb565; N], x0: i32, y0: i32, x1: i32, y1: i32, color: Rgb565) {
    for y in y0..=y1 {
        for x in x0..=x1 {
            plot(fb, x, y, color);
        }
    }
}

// 3x5 digit bitmaps (bit 2 = left, bit 1 = middle, bit 0 = right column).
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111], // 0
    [0b010, 0b110, 0b010, 0b010, 0b111], // 1
    [0b111, 0b001, 0b111, 0b100, 0b111], // 2
    [0b111, 0b001, 0b111, 0b001, 0b111], // 3
    [0b101, 0b101, 0b111, 0b001, 0b001], // 4
    [0b111, 0b100, 0b111, 0b001, 0b111], // 5
    [0b111, 0b100, 0b111, 0b101, 0b111], // 6
    [0b111, 0b001, 0b001, 0b010, 0b010], // 7
    [0b111, 0b101, 0b111, 0b101, 0b111], // 8
    [0b111, 0b101, 0b111, 0b001, 0b111], // 9
];

fn draw_digit(fb: &mut [Rgb565; N], x: i32, y: i32, digit: u8, scale: i32, color: Rgb565) {
    let rows = &DIGITS[(digit % 10) as usize];
    for (r, row) in rows.iter().enumerate() {
        for c in 0..3 {
            if row & (1 << (2 - c)) != 0 {
                fill_rect(
                    fb,
                    x + c * scale,
                    y + r as i32 * scale,
                    x + c * scale + scale - 1,
                    y + r as i32 * scale + scale - 1,
                    color,
                );
            }
        }
    }
}

fn draw_colon(fb: &mut [Rgb565; N], x: i32, y: i32, scale: i32, color: Rgb565) {
    fill_rect(fb, x, y + scale, x + scale - 1, y + 2 * scale - 1, color);
    fill_rect(fb, x, y + 3 * scale, x + scale - 1, y + 4 * scale - 1, color);
}

/// Draw `mm:ss` (or `m:ss`). Returns the x where it ended.
fn draw_mmss(fb: &mut [Rgb565; N], x: i32, y: i32, secs: u32, scale: i32, color: Rgb565) -> i32 {
    let m = secs / 60;
    let s = secs % 60;
    let mut cx = x;
    if m >= 10 {
        draw_digit(fb, cx, y, (m / 10) as u8, scale, color);
        cx += 4 * scale;
    }
    draw_digit(fb, cx, y, (m % 10) as u8, scale, color);
    cx += 4 * scale;
    draw_colon(fb, cx, y, scale, color);
    cx += 2 * scale;
    draw_digit(fb, cx, y, (s / 10) as u8, scale, color);
    cx += 4 * scale;
    draw_digit(fb, cx, y, (s % 10) as u8, scale, color);
    cx += 4 * scale;
    cx
}

/// Center `mm:ss` text horizontally.
fn draw_mmss_centered(fb: &mut [Rgb565; N], y: i32, secs: u32, scale: i32, color: Rgb565) {
    let m = secs / 60;
    // width in px = (minutes digits + colon + 2 seconds digits)
    let mins_digits = if m >= 10 { 2 } else { 1 };
    let width = (mins_digits * 4 + 2 + 2 * 4) * scale;
    let x = (WIDTH - width) / 2;
    draw_mmss(fb, x, y, secs, scale, color);
}

static mut FRAMEBUFFER: [Rgb565; N] = [Rgb565::BLACK; N];
static SPI_BUF: StaticCell<[u8; 4096]> = StaticCell::new();
static MONO_BUF: StaticCell<[u8; CHUNK]> = StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(_spawner: embassy_executor::Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // --- parse WAV ---
    let wav = parse_wav(WAV);
    let total_samples = wav.data.len() / 2;
    let total_secs = (total_samples / wav.sample_rate as usize) as u32;
    info!(
        "WAV: {} Hz, {} bytes, {} s",
        wav.sample_rate,
        wav.data.len(),
        total_secs
    );

    // --- display ---
    info!("Setting up ILI9341...");
    let spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(SPI_FREQ_MHZ))
            .with_mode(Mode::_0),
    )
    .unwrap()
    .with_sck(gpio(PIN_SCLK))
    .with_mosi(gpio(PIN_MOSI));

    let cs = Output::new(gpio(PIN_CS), Level::High, OutputConfig::default());
    let dc = Output::new(gpio(PIN_DC), Level::Low, OutputConfig::default());
    let rst = Output::new(gpio(PIN_RST), Level::Low, OutputConfig::default());
    let mut backlight = Output::new(gpio(PIN_BL), Level::Low, OutputConfig::default());
    backlight.set_high();

    let spi_dev = ExclusiveDevice::new_no_delay(spi, cs).unwrap();
    let mut delay = Delay::new();
    let buffer = SPI_BUF.init([0u8; 4096]);
    let di = SpiInterface::new(spi_dev, dc, buffer);
    let mut display = Builder::new(ILI9341Rgb565, di)
        .reset_pin(rst)
        .color_order(ColorOrder::Bgr)
        .orientation(Orientation::new())
        .init(&mut delay)
        .unwrap();

    // --- I2S audio (MAX98357A) ---
    info!("Setting up I2S...");
    let i2s = I2s::new(
        peripherals.I2S0,
        peripherals.DMA_CH0,
        TdmConfig::new_tdm_philips()
            .with_sample_rate(Rate::from_hz(wav.sample_rate))
            .with_data_format(DataFormat::Data16Channel16)
            .with_channels(Channels::MONO),
    )
    .unwrap();

    let mut i2s_tx = i2s
        .i2s_tx
        .with_bclk(gpio(PIN_I2S_BCLK))
        .with_ws(gpio(PIN_I2S_WS))
        .with_dout(gpio(PIN_I2S_DIN))
        .build();

    let mut dma_buf = dma_tx_buffer!(CHUNK).unwrap();
    let mut pos = 0usize;

    let fb = unsafe { &mut *addr_of_mut!(FRAMEBUFFER) };
    let mono = MONO_BUF.init([0u8; CHUNK]);

    info!("Playing (loop).");

    // Only repaint the progress UI when the shown second changes (the bar is
    // static otherwise) — keeps the display's SPI quiet so it doesn't inject
    // noise into the Class-D amp's supply.
    let mut last_drawn_sec = u32::MAX;

    loop {
        // Read the next chunk of PCM into a scratch buffer (looping the track).
        {
            let remaining = wav.data.len() - pos;
            if remaining >= CHUNK {
                mono.copy_from_slice(&wav.data[pos..pos + CHUNK]);
                pos += CHUNK;
            } else {
                mono[..remaining].copy_from_slice(&wav.data[pos..]);
                mono[remaining..].copy_from_slice(&wav.data[..CHUNK - remaining]);
                pos = CHUNK - remaining;
            }
        }

        // Scale (attenuate) and write into the DMA buffer.
        {
            let s = dma_buf.as_mut_slice();
            for i in 0..(CHUNK / 2) {
                let sample = i16::from_le_bytes([mono[2 * i], mono[2 * i + 1]]);
                let scaled = sample >> VOLUME_SHIFT;
                let b = scaled.to_le_bytes();
                s[2 * i] = b[0];
                s[2 * i + 1] = b[1];
            }
        }
        dma_buf.set_length(CHUNK);

        // Start the DMA transfer (audio plays while we draw).
        let transfer = i2s_tx.write(dma_buf).unwrap();

        let elapsed_secs = ((pos / 2) / wav.sample_rate as usize) as u32;

        if elapsed_secs != last_drawn_sec {
            last_drawn_sec = elapsed_secs;

            // --- render progress ---
            let frac = pos as u64 * 100 / wav.data.len() as u64; // percent

            fb.fill(Rgb565::BLACK);

            // Elapsed time (big).
            draw_mmss_centered(fb, 40, elapsed_secs, 6, Rgb565::WHITE);

            // Progress bar (border + fill).
            const BX0: i32 = 20;
            const BX1: i32 = 220;
            const BY0: i32 = 150;
            const BY1: i32 = 172;
            for x in BX0..=BX1 {
                plot(fb, x, BY0, Rgb565::WHITE);
                plot(fb, x, BY1, Rgb565::WHITE);
            }
            for y in BY0..=BY1 {
                plot(fb, BX0, y, Rgb565::WHITE);
                plot(fb, BX1, y, Rgb565::WHITE);
            }
            let fill_w = (frac * (BX1 - BX0 - 2) as u64 / 100) as i32;
            if fill_w > 0 {
                fill_rect(fb, BX0 + 1, BY0 + 1, BX0 + 1 + fill_w - 1, BY1 - 1, Rgb565::CYAN);
            }

            // Total time (small, dim).
            draw_mmss_centered(fb, 200, total_secs, 3, Rgb565::new(12, 24, 12));

            display
                .set_pixels(0, 0, (WIDTH - 1) as u16, (HEIGHT - 1) as u16, fb.iter().copied())
                .unwrap();
        }

        // Wait for this chunk to finish transmitting.
        let (_, tx, buf) = transfer.wait();
        i2s_tx = tx;
        dma_buf = buf;
    }
}
