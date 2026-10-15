#![no_std]
#![no_main]

use embassy_executor::Spawner;
use embassy_rp::bind_interrupts;
use embassy_rp::gpio::{Input, Level, Output, Pull};
use embassy_rp::i2c::{Blocking as I2cBlocking, Config as I2cConfig, I2c};
use embassy_rp::peripherals::{I2C0, SPI0, USB};
use embassy_rp::spi::{self, Blocking as SpiBlocking, Config as SpiConfig, Spi};
use embassy_rp::usb::{Driver, InterruptHandler as UsbInterruptHandler};
use embassy_time::{Duration, Timer};
use embassy_usb::class::midi::{MidiClass, USB_AUDIO_CLASS};
use embassy_usb::{Builder, Config as UsbConfig, UsbDevice};
use static_cell::StaticCell;
use vl53l0x::VL53L0x;

use panic_halt as _;

const MIDI_CHANNEL: u8 = 0;
const CC_DISTANCE: u8 = 74;
const BUTTON_NOTES: [u8; 4] = [36, 38, 42, 46];
const BUTTON_VELOCITY: u8 = 110;
const DISTANCE_MIN_MM: u16 = 40;
const DISTANCE_MAX_MM: u16 = 450;
const LCD_COL_OFFSET: u8 = 4;
const LCD_ROW_OFFSET: u8 = 3;

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => UsbInterruptHandler<USB>;
});

type UsbDriver = Driver<'static, USB>;
type LumentraUsb = UsbDevice<'static, UsbDriver>;
type LcdSpi = Spi<'static, SPI0, SpiBlocking>;
type TofI2c = I2c<'static, I2C0, I2cBlocking>;

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let p = embassy_rp::init(Default::default());

    let mut buttons = [
        Button::new(Input::new(p.PIN_10, Pull::Up), 0, BUTTON_NOTES[0]),
        Button::new(Input::new(p.PIN_11, Pull::Up), 1, BUTTON_NOTES[1]),
        Button::new(Input::new(p.PIN_12, Pull::Up), 2, BUTTON_NOTES[2]),
        Button::new(Input::new(p.PIN_13, Pull::Up), 3, BUTTON_NOTES[3]),
    ];

    let mut buzzer = Output::new(p.PIN_15, Level::Low);

    let driver = Driver::new(p.USB, Irqs);
    let mut builder = usb_builder(driver);
    let mut midi = MidiClass::new(&mut builder, 1, 1, 64);
    let usb = builder.build();
    spawner.spawn(usb_task(usb).unwrap());

    let mut display = {
        let mut cfg = SpiConfig::default();
        cfg.frequency = 16_000_000;
        cfg.phase = spi::Phase::CaptureOnSecondTransition;
        cfg.polarity = spi::Polarity::IdleHigh;

        let spi = Spi::new_blocking_txonly(p.SPI0, p.PIN_18, p.PIN_19, cfg);
        let dc = Output::new(p.PIN_16, Level::Low);
        let rst = Output::new(p.PIN_20, Level::High);
        Display::new(spi, dc, rst)
    };
    display.init().await;
    display.show_startup();

    let mut xshut = Output::new(p.PIN_6, Level::Low);
    xshut.set_low();
    Timer::after(Duration::from_millis(10)).await;
    xshut.set_high();
    Timer::after(Duration::from_millis(50)).await;

    let mut i2c_cfg = I2cConfig::default();
    i2c_cfg.frequency = 400_000;
    let mut i2c = I2c::new_blocking(p.I2C0, p.PIN_5, p.PIN_4, i2c_cfg);
    let mut tof = None;
    if i2c_device_present(&mut i2c, 0x29) {
        tof = VL53L0x::new(i2c).ok();
    }
    if let Some(sensor) = tof.as_mut() {
        let _ = sensor.set_measurement_timing_budget(33_000);
    }

    let mut last_cc = 255;
    let mut last_display_cc = 255;
    let mut button_states = [false; 4];

    loop {
        midi.wait_connection().await;
        startup_beep(&mut buzzer).await;
        display.draw_dashboard(&button_states, last_cc);

        while poll_controls(
            &mut midi,
            &mut buttons,
            tof.as_mut(),
            &mut buzzer,
            &mut display,
            &mut last_cc,
            &mut last_display_cc,
            &mut button_states,
        )
        .await
        .is_ok()
        {}

        all_notes_off(&mut midi).await.ok();
    }
}

fn usb_builder(driver: UsbDriver) -> Builder<'static, UsbDriver> {
    let mut config = UsbConfig::new(0x1209, 0x4D44);
    config.manufacturer = Some("Sandu Tudor Nicolas");
    config.product = Some("Lumentra USB MIDI");
    config.serial_number = Some("LUMENTRA-001");
    config.device_class = USB_AUDIO_CLASS;
    config.device_sub_class = 0x00;
    config.device_protocol = 0x00;
    config.composite_with_iads = false;
    config.max_power = 100;
    config.max_packet_size_0 = 64;

    static CONFIG_DESCRIPTOR: StaticCell<[u8; 512]> = StaticCell::new();
    static BOS_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();

    Builder::new(
        driver,
        config,
        CONFIG_DESCRIPTOR.init([0; 512]),
        BOS_DESCRIPTOR.init([0; 256]),
        &mut [],
        CONTROL_BUF.init([0; 64]),
    )
}

fn i2c_device_present(i2c: &mut TofI2c, address: u8) -> bool {
    let mut model_id = [0u8; 1];
    i2c.blocking_write_read(address, &[0xC0], &mut model_id)
        .is_ok()
}

#[embassy_executor::task]
async fn usb_task(mut usb: LumentraUsb) -> ! {
    usb.run().await
}

async fn poll_controls(
    midi: &mut MidiClass<'static, UsbDriver>,
    buttons: &mut [Button; 4],
    tof: Option<&mut VL53L0x<TofI2c>>,
    buzzer: &mut Output<'static>,
    display: &mut Display,
    last_cc: &mut u8,
    last_display_cc: &mut u8,
    button_states: &mut [bool; 4],
) -> Result<(), Disconnected> {
    for button in buttons {
        if let Some(event) = button.poll() {
            match event {
                ButtonEvent::Pressed { index, note } => {
                    button_states[index as usize] = true;
                    display.draw_button(index, true);
                    click_beep(buzzer).await;
                    midi_note_on(midi, note, BUTTON_VELOCITY).await?;
                }
                ButtonEvent::Released { index, note } => {
                    button_states[index as usize] = false;
                    display.draw_button(index, false);
                    midi_note_off(midi, note).await?;
                }
            }
        }
    }

    if let Some(sensor) = tof {
        if let Ok(mm) = sensor.read_range_single_millimeters_blocking() {
            let cc = distance_to_cc(mm);
            if cc.abs_diff(*last_cc) >= 2 {
                *last_cc = cc;
                midi_control_change(midi, CC_DISTANCE, cc).await?;
            }
        }
    }

    if *last_cc != *last_display_cc {
        *last_display_cc = *last_cc;
        display.draw_cc(*last_cc);
    }

    Timer::after(Duration::from_millis(6)).await;
    Ok(())
}

async fn midi_note_on(
    midi: &mut MidiClass<'static, UsbDriver>,
    note: u8,
    velocity: u8,
) -> Result<(), Disconnected> {
    midi.write_packet(&[0x09, 0x90 | MIDI_CHANNEL, note, velocity])
        .await
        .map_err(Disconnected::from)
}

async fn midi_note_off(
    midi: &mut MidiClass<'static, UsbDriver>,
    note: u8,
) -> Result<(), Disconnected> {
    midi.write_packet(&[0x08, 0x80 | MIDI_CHANNEL, note, 0])
        .await
        .map_err(Disconnected::from)
}

async fn midi_control_change(
    midi: &mut MidiClass<'static, UsbDriver>,
    cc: u8,
    value: u8,
) -> Result<(), Disconnected> {
    midi.write_packet(&[0x0B, 0xB0 | MIDI_CHANNEL, cc, value])
        .await
        .map_err(Disconnected::from)
}

async fn all_notes_off(midi: &mut MidiClass<'static, UsbDriver>) -> Result<(), Disconnected> {
    for note in BUTTON_NOTES {
        midi_note_off(midi, note).await?;
    }
    Ok(())
}

fn distance_to_cc(mm: u16) -> u8 {
    let clamped = mm.clamp(DISTANCE_MIN_MM, DISTANCE_MAX_MM);
    let span = DISTANCE_MAX_MM - DISTANCE_MIN_MM;
    let shifted = clamped - DISTANCE_MIN_MM;
    127 - ((shifted as u32 * 127 / span as u32) as u8)
}

async fn startup_beep(pin: &mut Output<'static>) {
    tone(pin, 880, 55).await;
    Timer::after(Duration::from_millis(35)).await;
    tone(pin, 1320, 55).await;
}

async fn click_beep(pin: &mut Output<'static>) {
    tone(pin, 1800, 18).await;
}

async fn tone(pin: &mut Output<'static>, frequency_hz: u32, duration_ms: u32) {
    let half_period_us = 500_000 / frequency_hz;
    let cycles = duration_ms * 1000 / (half_period_us * 2);

    for _ in 0..cycles {
        pin.set_high();
        Timer::after(Duration::from_micros(half_period_us as u64)).await;
        pin.set_low();
        Timer::after(Duration::from_micros(half_period_us as u64)).await;
    }
}

struct Button {
    pin: Input<'static>,
    index: u8,
    note: u8,
    previous: bool,
}

impl Button {
    fn new(pin: Input<'static>, index: u8, note: u8) -> Self {
        Self {
            pin,
            index,
            note,
            previous: false,
        }
    }

    fn poll(&mut self) -> Option<ButtonEvent> {
        let now = self.pin.is_low();
        if now == self.previous {
            return None;
        }

        self.previous = now;
        if now {
            Some(ButtonEvent::Pressed {
                index: self.index,
                note: self.note,
            })
        } else {
            Some(ButtonEvent::Released {
                index: self.index,
                note: self.note,
            })
        }
    }
}

enum ButtonEvent {
    Pressed { index: u8, note: u8 },
    Released { index: u8, note: u8 },
}

struct Display {
    spi: LcdSpi,
    dc: Output<'static>,
    rst: Output<'static>,
}

impl Display {
    fn new(spi: LcdSpi, dc: Output<'static>, rst: Output<'static>) -> Self {
        Self { spi, dc, rst }
    }

    async fn init(&mut self) {
        self.rst.set_low();
        Timer::after(Duration::from_millis(20)).await;
        self.rst.set_high();
        Timer::after(Duration::from_millis(150)).await;

        self.command(0x01);
        Timer::after(Duration::from_millis(150)).await;
        self.command(0x11);
        Timer::after(Duration::from_millis(120)).await;
        self.command_with_data(0x3A, &[0x05]);
        self.command_with_data(0x36, &[0xA8]);
        self.command(0x29);
        Timer::after(Duration::from_millis(50)).await;
        self.clear(0x0000);
    }

    fn show_startup(&mut self) {
        self.draw_dashboard(&[false; 4], 0);
    }

    fn draw_dashboard(&mut self, buttons: &[bool; 4], cc: u8) {
        self.clear(0x0000);

        self.rect(4, 4, 116, 1, 0x4208);
        self.rect(4, 119, 116, 1, 0x4208);
        self.rect(4, 4, 1, 116, 0x4208);
        self.rect(119, 4, 1, 116, 0x4208);
        for i in 0..4 {
            self.draw_button(i, buttons[i as usize]);
        }

        self.draw_cc(cc);
    }

    fn draw_cc(&mut self, cc: u8) {
        let fill = ((cc as u16 * 96) / 127) as u8;
        let fill_x = 16 + (96 - fill);

        self.rect(0, 88, 120, 28, 0x0000);
        self.rect(14, 94, 100, 18, 0x18E3);
        if fill > 0 {
            self.rect(fill_x, 97, fill, 12, 0x07E0);
        }
    }

    fn draw_button(&mut self, index: u8, pressed: bool) {
        const COLORS: [u16; 4] = [0xF800, 0x07E0, 0x001F, 0xFFE0];

        let x = 18 + index * 26;
        let color = if pressed {
            COLORS[index as usize]
        } else {
            0x39E7
        };

        self.rect(x, 16, 17, 58, color);
        if !pressed {
            self.rect(x + 3, 19, 11, 52, 0x8410);
        }
    }

    fn clear(&mut self, color: u16) {
        self.raw_set_window(0, 0, 131, 131);
        self.fill_pixels(132 * 132, color);

        self.set_window(0, 0, 127, 127);
        self.fill_pixels(128 * 128, color);
    }

    fn rect(&mut self, x: u8, y: u8, w: u8, h: u8, color: u16) {
        if w == 0 || h == 0 {
            return;
        }

        if x >= 128 || y >= 128 {
            return;
        }

        let x1 = (x as u16 + w as u16 - 1).min(127) as u8;
        let y1 = (y as u16 + h as u16 - 1).min(127) as u8;
        let w = x1 - x + 1;
        let h = y1 - y + 1;

        self.set_window(x, y, x1, y1);
        self.fill_pixels(w as u16 * h as u16, color);
    }

    fn fill_pixels(&mut self, pixels: u16, color: u16) {
        self.dc.set_high();
        let hi = (color >> 8) as u8;
        let lo = color as u8;
        let mut buf = [0u8; 64];
        for chunk in buf.chunks_exact_mut(2) {
            chunk[0] = hi;
            chunk[1] = lo;
        }

        let mut bytes_left = pixels as u32 * 2;
        while bytes_left >= buf.len() as u32 {
            self.spi.blocking_write(&buf).ok();
            bytes_left -= buf.len() as u32;
        }

        if bytes_left > 0 {
            self.spi.blocking_write(&buf[..bytes_left as usize]).ok();
        }
    }

    fn set_window(&mut self, x0: u8, y0: u8, x1: u8, y1: u8) {
        let x0 = x0 + LCD_COL_OFFSET;
        let x1 = x1 + LCD_COL_OFFSET;
        let y0 = y0 + LCD_ROW_OFFSET;
        let y1 = y1 + LCD_ROW_OFFSET;

        self.command_with_data(0x2A, &[0, x0, 0, x1]);
        self.command_with_data(0x2B, &[0, y0, 0, y1]);
        self.command(0x2C);
    }

    fn raw_set_window(&mut self, x0: u8, y0: u8, x1: u8, y1: u8) {
        self.command_with_data(0x2A, &[0, x0, 0, x1]);
        self.command_with_data(0x2B, &[0, y0, 0, y1]);
        self.command(0x2C);
    }

    fn command(&mut self, cmd: u8) {
        self.dc.set_low();
        self.spi.blocking_write(&[cmd]).ok();
    }

    fn command_with_data(&mut self, cmd: u8, data: &[u8]) {
        self.command(cmd);
        self.dc.set_high();
        self.spi.blocking_write(data).ok();
    }
}

struct Disconnected;

impl From<embassy_usb::driver::EndpointError> for Disconnected {
    fn from(value: embassy_usb::driver::EndpointError) -> Self {
        match value {
            embassy_usb::driver::EndpointError::BufferOverflow => loop {
                cortex_m::asm::bkpt();
            },
            embassy_usb::driver::EndpointError::Disabled => Disconnected,
        }
    }
}
