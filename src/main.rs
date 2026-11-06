#![no_std]
#![no_main]

use core::ptr::addr_of_mut;

use embassy_time::{Duration, Instant, Timer};
use embedded_graphics_core::pixelcolor::{Rgb565, RgbColor};
use embedded_hal_bus::spi::ExclusiveDevice;
use esp_backtrace as _;
use esp_hal::analog::adc::{Adc, AdcConfig, Attenuation};
use esp_hal::clock::CpuClock;
use esp_hal::delay::Delay;
use esp_hal::gpio::{AnyPin, Input, InputConfig, Level, Output, OutputConfig, Pull};
use esp_hal::spi::master::{Config as SpiConfig, Spi};
use esp_hal::spi::Mode;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use log::info;
use mipidsi::options::{ColorOrder, Orientation};
use mipidsi::{interface::SpiInterface, models::ILI9341Rgb565, Builder};
use static_cell::StaticCell;

// =============================================================================
// Configuration — the single place to change wiring.
// =============================================================================

// --- Display (ILI9341 over SPI) ---
const PIN_SCLK: u8 = 12;
const PIN_MOSI: u8 = 11;
const PIN_CS: u8 = 10;
const PIN_DC: u8 = 9;
const PIN_RST: u8 = 14;
const PIN_BL: u8 = 2;

// --- Joystick (PS2 module). VRx/VRy must be ADC1 pins (GPIO1..10). ---
const PIN_VRX: u8 = 4; // X axis -> ADC1_CH3
const PIN_VRY: u8 = 5; // Y axis -> ADC1_CH4
const PIN_SW: u8 = 6; // button (active low, internal pull-up)

const SPI_FREQ_MHZ: u32 = 40;

const WIDTH: i32 = 240;
const HEIGHT: i32 = 320;
const N: usize = (WIDTH * HEIGHT) as usize;

// --- Display geometry ---
const CX: i32 = WIDTH / 2; // 120
const CY: i32 = HEIGHT / 2; // 160
const RING_RADIUS: i32 = 100; // the circle's radius
const RING_THICKNESS: i32 = 2;
const DOT_RADIUS: i32 = 6;
const DEADZONE_PX: i32 = 4; // joystick jitter deadzone (pixels)

// Resolve a const GPIO number to a pin handle (for digital pins).
fn gpio(n: u8) -> AnyPin<'static> {
    unsafe { AnyPin::steal(n) }
}

// Map a raw ADC reading to a pixel offset in [-RING_RADIUS, RING_RADIUS].
//
// Joystick pots are often asymmetric (the resting point isn't the midpoint of
// the observed range), so each direction is scaled by its own observed extent
// (`center - min` for negative, `max - center` for positive). This way the full
// physical throw maps to the full circle in every direction.
fn map_axis(raw: i32, center: i32, min: i32, max: i32) -> i32 {
    let d = raw - center;
    let range = if d >= 0 { max - center } else { center - min };
    let v = d * RING_RADIUS / range.max(1);
    if v.abs() < DEADZONE_PX {
        return 0;
    }
    v.clamp(-RING_RADIUS, RING_RADIUS)
}

/// Clamp the (dx, dy) offset vector to a circle of radius `max_r`, so the dot
/// stays inside the ring instead of reaching the corners of the bounding box.
fn clip_to_circle(dx: i32, dy: i32, max_r: i32) -> (i32, i32) {
    let r2 = (dx * dx + dy * dy) as f32;
    let max2 = (max_r * max_r) as f32;
    if r2 <= max2 {
        return (dx, dy);
    }
    let scale = max_r as f32 / libm::sqrtf(r2);
    ((dx as f32 * scale) as i32, (dy as f32 * scale) as i32)
}

fn plot(fb: &mut [Rgb565; N], x: i32, y: i32, color: Rgb565) {
    if x >= 0 && x < WIDTH && y >= 0 && y < HEIGHT {
        fb[y as usize * WIDTH as usize + x as usize] = color;
    }
}

/// Ring outline (band of radius `r` ± `thickness`).
fn draw_ring(fb: &mut [Rgb565; N], cx: i32, cy: i32, r: i32, thickness: i32, color: Rgb565) {
    let outer = r + thickness;
    let inner = (r - thickness).max(0);
    for dy in -outer..=outer {
        for dx in -outer..=outer {
            let d2 = dx * dx + dy * dy;
            if d2 <= outer * outer && d2 >= inner * inner {
                plot(fb, cx + dx, cy + dy, color);
            }
        }
    }
}

/// Filled circle.
fn draw_dot(fb: &mut [Rgb565; N], cx: i32, cy: i32, r: i32, color: Rgb565) {
    for dy in -r..=r {
        for dx in -r..=r {
            if dx * dx + dy * dy <= r * r {
                plot(fb, cx + dx, cy + dy, color);
            }
        }
    }
}

static mut FRAMEBUFFER: [Rgb565; N] = [Rgb565::BLACK; N];
static SPI_BUF: StaticCell<[u8; 4096]> = StaticCell::new();

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_rtos::main]
async fn main(_spawner: embassy_executor::Spawner) -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(timg0.timer0, peripherals.FROM_CPU_INTR0);

    info!("Setting up ILI9341...");

    // --- display ---
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

    // --- joystick ---
    info!("Setting up joystick (VRx=GPIO{}, VRy=GPIO{}, SW=GPIO{})...", PIN_VRX, PIN_VRY, PIN_SW);

    // ADC (VRx/VRy) needs typed GPIO pins.
    let mut adc_config = AdcConfig::new();
    let mut vrx = adc_config.enable_pin(peripherals.GPIO4, Attenuation::_11dB);
    let mut vry = adc_config.enable_pin(peripherals.GPIO5, Attenuation::_11dB);
    let mut adc = Adc::new(peripherals.ADC1, adc_config);

    // Button (SW): active-low, use internal pull-up.
    let sw = Input::new(gpio(PIN_SW), InputConfig::default().with_pull(Pull::Up));

    // Warm up the ADC and discard the first (possibly stale) readings.
    for _ in 0..8 {
        let _ = nb::block!(adc.read_oneshot(&mut vrx));
        let _ = nb::block!(adc.read_oneshot(&mut vry));
    }

    // Auto-center: average the resting position (keep the stick centered for
    // the first second after boot).
    let mut sum_x = 0u32;
    let mut sum_y = 0u32;
    const CAL_SAMPLES: usize = 32;
    for _ in 0..CAL_SAMPLES {
        sum_x += nb::block!(adc.read_oneshot(&mut vrx)).unwrap() as u32;
        sum_y += nb::block!(adc.read_oneshot(&mut vry)).unwrap() as u32;
    }
    let center_x = (sum_x / CAL_SAMPLES as u32) as i32;
    let center_y = (sum_y / CAL_SAMPLES as u32) as i32;
    info!("Joystick center: ({}, {})", center_x, center_y);

    let fb = unsafe { &mut *addr_of_mut!(FRAMEBUFFER) };

    info!("Ready.");

    // Track each axis's observed min/max at runtime, starting from the center.
    // This adapts to the joystick's actual (possibly asymmetric) range.
    let mut min_x = center_x;
    let mut max_x = center_x;
    let mut min_y = center_y;
    let mut max_y = center_y;

    let mut last_log = Instant::now();

    loop {
        let x = nb::block!(adc.read_oneshot(&mut vrx)).unwrap() as i32;
        let y = nb::block!(adc.read_oneshot(&mut vry)).unwrap() as i32;
        let pressed = sw.is_low();

        // Expand the observed range as the stick moves.
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);

        let dx = map_axis(x, center_x, min_x, max_x);
        let dy = map_axis(y, center_y, min_y, max_y);
        // Clip the dot's center to the SAME radius as the drawn ring.
        let (dx, dy) = clip_to_circle(dx, dy, RING_RADIUS);
        let dot_x = CX + dx;
        let dot_y = CY + dy;

        // Render.
        fb.fill(Rgb565::BLACK);
        draw_ring(fb, CX, CY, RING_RADIUS, RING_THICKNESS, Rgb565::WHITE);
        // Dot: cyan normally, red while pressed.
        let dot_color = if pressed { Rgb565::RED } else { Rgb565::CYAN };
        draw_dot(fb, dot_x, dot_y, DOT_RADIUS, dot_color);

        display
            .set_pixels(0, 0, (WIDTH - 1) as u16, (HEIGHT - 1) as u16, fb.iter().copied())
            .unwrap();

        if last_log.elapsed() >= Duration::from_secs(2) {
            info!(
                "raw=({},{}) range=({}..{},{}..{}) dot=({},{})",
                x, y, min_x, max_x, min_y, max_y, dot_x, dot_y
            );
            last_log = Instant::now();
        }

        Timer::after(Duration::from_millis(5)).await;
    }
}
