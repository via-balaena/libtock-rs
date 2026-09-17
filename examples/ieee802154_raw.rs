//! An example showing use of IEEE 802.15.4 networking.
//!
//! The kernel contains a standard and phy 15.4 driver. This example
//! expects the kernel to be configured with the phy 15.4 driver to
//! allow direct access to the radio and the ability to send "raw"
//! frames. An example board file using this driver is provided at
//! `boards/tutorials/nrf52840dk-thread-tutorial`.
//!
//! "No Support" Errors for setting the channel/tx power are a telltale
//! sign that the kernel is not configured with the phy 15.4 driver.

#![no_main]
#![no_std]
use core::fmt::Write;
use libtock::console::Console;
use libtock::ieee802154::{Ieee802154, RxOperator as _, RxRingBuffer, RxSingleBufferOperator};
use libtock::runtime::{set_main, stack_size};

set_main! {main}
stack_size! {0x600}

fn main() {
    // Configure the radio
    let pan: u16 = 0xcafe;
    let addr_short: u16 = 0xdead;
    let addr_long: u64 = 0x0dea_ddad;
    let tx_power: i8 = -3;
    let channel: u8 = 11;

    writeln!(Console::writer(), "Configuring IEEE 802.15.4 radio...\n").unwrap();

    Ieee802154::set_pan(pan);
    writeln!(Console::writer(), "Set PAN to {pan:#06x}\n").unwrap();

    Ieee802154::set_address_short(addr_short);
    writeln!(
        Console::writer(),
        "Set short address to {addr_short:#06x}\n"
    )
    .unwrap();

    Ieee802154::set_address_long(addr_long);
    writeln!(Console::writer(), "Set long address to {addr_long:#018x}\n").unwrap();

    Ieee802154::set_tx_power(tx_power).unwrap();
    writeln!(Console::writer(), "Set TX power to {tx_power}\n").unwrap();

    Ieee802154::set_channel(channel).unwrap();
    writeln!(Console::writer(), "Set channel to {channel}\n").unwrap();

    // Don't forget to commit the config!
    Ieee802154::commit_config();
    writeln!(Console::writer(), "Committed radio configuration!\n").unwrap();

    // Turn the radio on
    Ieee802154::radio_on().unwrap();
    assert!(Ieee802154::is_on());

    // Transmit a frame
    Ieee802154::transmit_frame_raw(b"foobar").unwrap();

    Console::write(b"Transmitted frame!\n").unwrap();

    // Showcase receiving to a single buffer - there is a risk of losing some frames.
    // See [RxSingleBufferOperator] docs for more details.
    rx_single_buffer();
}

fn rx_single_buffer() {
    let mut buf = RxRingBuffer::<2>::new();
    let mut operator = RxSingleBufferOperator::new(&mut buf);

    let frame1 = operator.receive_frame().unwrap();
    // Access frame1 data here:
    let _body_len = frame1.payload_len;
    let _first_body_byte = frame1.body[0];

    let _frame2 = operator.receive_frame().unwrap();
    // Access frame2 data here
}
