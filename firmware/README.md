# Lumentra firmware

Rust firmware for a Raspberry Pi Pico 2 W USB-MIDI controller.

## Hardware pins

| Part | Pico 2 W pins |
|------|---------------|
| Buttons | GP10, GP11, GP12, GP13 |
| Buzzer | GP15 |
| LCD ST7735 | DC GP16, SCL GP18, SDA/MOSI GP19, RES GP20, CS GND, BLK 3.3V |
| VL53L0X | SDA GP4, SCL GP5, XSHUT GP6 |

The button code uses the Pico internal pull-up resistors. Each button should connect its GPIO pin to GND when pressed, so a pressed button reads LOW.

## Build

```powershell
rustup target add thumbv8m.main-none-eabihf
cargo build --release
```

To create a UF2 file, install `elf2uf2-rs`:

```powershell
cargo install elf2uf2-rs
.\scripts\make-uf2.ps1
```

Then hold BOOTSEL while plugging in the Pico 2 W and copy `lumentra-rp2350.uf2` to the mounted drive.

`elf2uf2-rs` defaults to the RP2040 UF2 family ID, so the script patches the generated UF2 to the RP2350 ARM secure family ID `0xE48BFF59`.
