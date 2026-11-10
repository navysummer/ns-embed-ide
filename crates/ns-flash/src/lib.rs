//! Firmware flashing: adapters for OpenOCD, pyOCD and J-Link Commander, plus
//! firmware format handling (ELF / Intel HEX / BIN).
//!
//! Everything is derived from `project.toml`'s `[flasher]` section; the IDE
//! never hardcodes a flasher path.

pub mod firmware;
pub mod flash_algorithm;
pub mod flasher;

pub use firmware::{DecodedHex, FirmwareFormat, FirmwareImage, FirmwareSize, analyze_firmware_size, convert_firmware, decode_intel_hex, load_firmware};
pub use flash_algorithm::{FlashAlgorithm, load_flm, parse_flm};
pub use flasher::{FlashAction, FlashOutcome, FlashRequest, FlashTool, Flasher, adapter_for};
