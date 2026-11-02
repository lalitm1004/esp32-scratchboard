#![no_std]
#![no_main]

mod donut;

use donut::{Donut, HEIGHT, WIDTH};

use embassy_time::{Duration, Instant, Timer};
use embedded_graphics_core::pixelcolor::{Rgb565, RgbColor};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{AnyPin, Level, Output, OutputConfig};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use log::info;
use mipidsi::options::{ColorOrder, Orientation};
use mipidsi::{interface::SpiInterface, models::ILI9341Rgb565, Builder};
use static_cell::StaticCell;

// =============================================================================
// Configuration — the single place to change wiring / speed.
// =============================================================================

// SPI pins (ESP32-S3 -> ILI9341).
const PIN_SCLK: u8 = 12; // SPI clock
const PIN_MOSI: u8 = 11; // SPI data out (SDA)
const PIN_CS: u8 = 10; // chip select
const PIN_DC: u8 = 9; // data/command
const PIN_RST: u8 = 14; // reset
const PIN_BL: u8 = 2; // backlight (driven high to enable)

const SPI_FREQ_MHZ: u32 = 40; // lower to 20 or 10 if you see glitches

const N: usize = WIDTH * HEIGHT;

// Resolve a const GPIO number to a pin handle. `steal` is safe here because
// each pin above is claimed exactly once (all six numbers are distinct).
fn gpio(n: u8) -> AnyPin<'static> {
    // Panics if `n` is not a real GPIO number on this chip.
    unsafe { AnyPin::steal(n) }
}

// Large frame buffers. Declared `static mut` with zero-initializers so the
// linker zero-fills them in `.bss` — constructing them by value (e.g. via
// `StaticCell::init([Rgb565::BLACK; N])`) would allocate a huge temporary on
// the stack and overflow the 84 KB main stack. `render()` writes every byte
// before anything is read, so access is safe in this single-threaded program.
static mut COLOR: [Rgb565; N] = [Rgb565::BLACK; N];
static mut DEPTH: [u8; N] = [0u8; N];
static SPI_BUF: StaticCell<[u8; 4096]> = StaticCell::new();
static DONUT: StaticCell<Donut> = StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(_spawner: embassy_executor::Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    info!("Embassy up. Setting up ILI9341 display...");

    // --- SPI bus (an embedded-hal `SpiBus`) ---
    let spi = Spi::new(
        peripherals.SPI2,
        SpiConfig::default()
            .with_frequency(Rate::from_mhz(SPI_FREQ_MHZ))
            .with_mode(Mode::_0),
    )
    .unwrap()
    .with_sck(gpio(PIN_SCLK))
    .with_mosi(gpio(PIN_MOSI));

    // --- control pins ---
    let cs = Output::new(gpio(PIN_CS), Level::High, OutputConfig::default());
    let dc = Output::new(gpio(PIN_DC), Level::Low, OutputConfig::default());
    let rst = Output::new(gpio(PIN_RST), Level::Low, OutputConfig::default());
    let mut backlight = Output::new(gpio(PIN_BL), Level::Low, OutputConfig::default());
    backlight.set_high();

    // Wrap the bus + CS pin into an embedded-hal `SpiDevice` (what mipidsi wants).
    let spi_dev = ExclusiveDevice::new_no_delay(spi, cs).unwrap();

    // --- display ---
    let mut delay = Delay::new();
    let buffer = SPI_BUF.init([0u8; 4096]);
    let di = SpiInterface::new(spi_dev, dc, buffer);

    let mut display = Builder::new(ILI9341Rgb565, di)
        .reset_pin(rst)
        .color_order(ColorOrder::Bgr)
        .orientation(Orientation::new())
        .init(&mut delay)
        .unwrap();

    info!("Display initialized, spinning up the donut.");

    let color = unsafe { &mut *core::ptr::addr_of_mut!(COLOR) };
    let depth = unsafe { &mut *core::ptr::addr_of_mut!(DEPTH) };
    let donut = DONUT.init(Donut::new());

    let mut last = Instant::now();
    let mut frames = 0u32;

    loop {
        donut.render(color, depth);

        display
            .set_pixels(0, 0, (WIDTH - 1) as u16, (HEIGHT - 1) as u16, color.iter().copied())
            .unwrap();

        frames += 1;
        if last.elapsed() >= Duration::from_secs(1) {
            info!("{} fps", frames);
            frames = 0;
            last = Instant::now();
        }

        // Yield to the executor (also keeps WDT happy if one is enabled).
        Timer::after(Duration::from_millis(1)).await;
    }
}
