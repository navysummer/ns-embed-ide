//! Firmware flashing: adapters for OpenOCD, pyOCD and J-Link Commander, plus
//! firmware format handling (ELF / Intel HEX / BIN).
//!
//! Everything is derived from `project.toml`'s `[flasher]` section; the IDE
//! never hardcodes a flasher path.

pub mod firmware;
pub mod flasher;

pub use firmware::{FirmwareFormat, FirmwareImage, convert_firmware, load_firmware};
pub use flasher::{FlashAction, FlashOutcome, FlashRequest, FlashTool, Flasher, adapter_for};
