//! A Pico's USB device on the computer's USB (USB/IP): the transfers of the
//! computer's host go to the Pico's thread, which plays them into its USB
//! controller ([`rp2040::usb::SerialPipe::link`]), and come back as
//! replies ([`usbip::forward`]). The device appears as the Pico would (its
//! firmware's descriptors, e.g. 2E8A:000A for pico-sdk's USB serial port).

pub use usbip::forward::{ForwardedDevice as PicoUsbDevice, SharedBridge};
