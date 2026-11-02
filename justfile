# justfile — ESP32-S3 donut
set shell := ["bash", "-uc"]

chip := "esp32s3"
bin  := "target/xtensa-esp32s3-none-elf/release/esp32"

# show available recipes
default:
    @just --list

# compile only — does NOT touch the device
build:
    cargo build --release

# flash the already-built binary (run `just build` first)
flash:
    @test -f "{{bin}}" || { echo "error: {{bin}} not found - run 'just build' first" >&2; exit 1; }
    espflash flash --chip {{chip}} {{bin}}

# open the serial monitor (assumes already flashed)
monitor:
    espflash monitor --chip {{chip}}

# flash + open the monitor (no rebuild)
flash-monitor:
    @test -f "{{bin}}" || { echo "error: {{bin}} not found - run 'just build' first" >&2; exit 1; }
    espflash flash --monitor --chip {{chip}} {{bin}}

# erase flash
erase:
    espflash erase-flash --chip {{chip}}

# clean build artifacts
clean:
    cargo clean
