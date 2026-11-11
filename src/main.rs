#![no_std]
#![no_main]

use esp_hal::clock::CpuClock;
use esp_hal::dma_tx_buffer;
use esp_hal::gpio::{AnyPin, Level, Output, OutputConfig};
use esp_hal::i2s::master::{Channels, DataFormat, I2s, TdmConfig};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_backtrace as _;
use esp_hal::delay::Delay;
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
const PIN_I2S_BCLK: u8 = 4;
const PIN_I2S_WS: u8 = 5;
const PIN_I2S_DIN: u8 = 6;

const SPI_FREQ_MHZ: u32 = 40;
const SAMPLE_RATE: u32 = 32000;
const CHUNK: usize = 4096; // bytes per DMA transfer (2048 samples)

// --- Beep (alarm: loud on/off pattern) ---
const BEEP_FREQ_HZ: u32 = 2000; // square-wave frequency during the "on" phase
const BEEP_AMP: i16 = i16::MAX; // 32767 = full scale = loudest
const BEEP_ON_MS: u32 = 300; // beep duration
const BEEP_OFF_MS: u32 = 300; // silence duration

fn gpio(n: u8) -> AnyPin<'static> {
    unsafe { AnyPin::steal(n) }
}

/// Fill the DMA buffer with an alarm pattern: a full-scale square wave for
/// `BEEP_ON_MS`, then silence for `BEEP_OFF_MS`, repeating. `t` is the running
/// sample position within the on/off cycle (kept in `[0, cycle)`).
fn fill_alarm(buf: &mut [u8], t: &mut u32) {
    let on = SAMPLE_RATE * BEEP_ON_MS / 1000;
    let off = SAMPLE_RATE * BEEP_OFF_MS / 1000;
    let cycle = on + off;
    let sq_period = (SAMPLE_RATE / BEEP_FREQ_HZ).max(2);
    let sq_half = sq_period / 2;

    for (i, s) in buf.chunks_exact_mut(2).enumerate() {
        let p = (*t + i as u32) % cycle;
        let v: i16 = if p < on {
            let sq = p % sq_period;
            if sq < sq_half {
                BEEP_AMP
            } else {
                -BEEP_AMP
            }
        } else {
            0
        };
        s.copy_from_slice(&v.to_le_bytes());
    }
    *t = (*t + (buf.len() / 2) as u32) % cycle;
}

static SPI_BUF: StaticCell<[u8; 4096]> = StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(_spawner: embassy_executor::Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    // --- display (cleared once; not otherwise used by the beep) ---
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
    let _display = Builder::new(ILI9341Rgb565, di)
        .reset_pin(rst)
        .color_order(ColorOrder::Bgr)
        .orientation(Orientation::new())
        .init(&mut delay)
        .unwrap();

    // --- I2S audio (MAX98357A) ---
    let i2s = I2s::new(
        peripherals.I2S0,
        peripherals.DMA_CH0,
        TdmConfig::new_tdm_philips()
            .with_sample_rate(Rate::from_hz(SAMPLE_RATE))
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

    info!(
        "Alarm: {} ms beep / {} ms silence at {} Hz (full scale).",
        BEEP_ON_MS, BEEP_OFF_MS, BEEP_FREQ_HZ
    );

    let mut t = 0u32; // position in the on/off cycle

    loop {
        fill_alarm(dma_buf.as_mut_slice(), &mut t);
        dma_buf.set_length(CHUNK);

        let transfer = i2s_tx.write(dma_buf).unwrap();
        let (_, tx, buf) = transfer.wait();
        i2s_tx = tx;
        dma_buf = buf;
    }
}
