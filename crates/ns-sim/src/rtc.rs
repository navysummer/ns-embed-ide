//! Virtual RTC (Real-Time Clock) peripheral model for ns-sim.
//!
//! Implements a memory-mapped RTC providing:
//! - Calendar timekeeping (seconds, minutes, hours, day, month, year)
//! - Alarm capability
//! - Sub-second precision (fractional seconds)
//! - Wake-up timer
//!
//! Register map (base + offset):
//!
//! | Offset   | Register       | Access | Description                              |
//! |----------|----------------|--------|------------------------------------------|
//! | `0x00`   | CR1            | RW     | Control register 1                       |
//! | `0x04`   | CR2            | RW     | Control register 2 (alarms, interrupts)  |
//! | `0x08`   | CR3            | RW     | Control register 3 (clock config)        |
//! | `0x0C`   | SR             | RO     | Status register                          |
//! | `0x10`   | PRER           | RW     | Prescaler register (async/sync)          |
//! | `0x14`   | DIVR           | RO     | Division factor register                 |
//! | `0x18`   | CNTH           | RW     | Counter high (milliseconds part)         |
//! | `0x1C`   |_CNTL           | RW     | Counter low (seconds part)               |
//! | `0x20`   | ALRMAR         | RW     | Alarm mask register                      |
//! | `0x24`   | ALRMBR         | RW     | Alarm B mask register                    |
//!
//! **Time format**: Binary Coded Decimal (BCD) for calendar values,
//! or binary mode configurable via CR1.

use serde::Serialize;

/// RTC prescaler configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RtcPrescaler {
    /// Async prescaler value (divides clock before sync counter).
    pub prediv_a: u16,
    /// Sync prescaler value (divides clock for second tick).
    pub prediv_b: u16,
}

impl Default for RtcPrescaler {
    fn default() -> Self {
        // Typical values for 1HzRTC clock from 32.768 kHz LSE
        Self {
            prediv_a: 127,  // 128 division for async
            prediv_b: 255,  // 256 division for sync (128*256 = 32768 -> 1 Hz)
        }
    }
}

impl RtcPrescaler {
    pub fn total_division(&self) -> u32 {
        (self.prediv_a as u32 + 1) * (self.prediv_b as u32 + 1)
    }
}

/// Calendar time fields in BCD format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RtcTime {
    /// Seconds (0-59, BCD encoded in storage but stored as decimal here).
    pub sec: u8,
    /// Minutes (0-59).
    pub min: u8,
    /// Hours (0-23).
    pub hour: u8,
    /// Day of week (1-7, Mon-Sun).
    pub wday: u8,
    /// Day of month (1-31).
    pub day: u8,
    /// Month (1-12).
    pub month: u8,
    /// Year (0-99).
    pub year: u8,
}

impl Default for RtcTime {
    fn default() -> Self {
        Self {
            sec: 0,
            min: 0,
            hour: 0,
            wday: 1,
            day: 1,
            month: 1,
            year: 0,
        }
    }
}

/// Status flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RtcSrFlags {
    pub rsf: bool,   // Registers synchronized flag
    pub calff: bool, // Calibration flag
    pub alrf: bool,  // Alarm flag
    pub wf: bool,    // Wakeup flag
    pub initf: bool, // Initialization flag
}

impl Default for RtcSrFlags {
    fn default() -> Self {
        Self {
            rsf: true,   // Start as synchronized
            calff: false,
            alrf: false,
            wf: false,
            initf: false,
        }
    }
}

/// Alarm configuration for alarm A or B.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RtcAlarm {
    pub enabled: bool,
    pub mask_sec: bool,
    pub mask_min: bool,
    pub mask_hour: bool,
    pub mask_day: bool,
    pub mask_month: bool,
    pub mask_wday: bool,
    pub target: RtcTime,
}

impl Default for RtcAlarm {
    fn default() -> Self {
        Self {
            enabled: false,
            mask_sec: true,
            mask_min: true,
            mask_hour: true,
            mask_day: false,
            mask_month: false,
            mask_wday: false,
            target: RtcTime::default(),
        }
    }
}

/// RTC peripheral state.
#[derive(Debug, Clone, Serialize)]
pub struct RtcPeripheral {
    pub base: u32,
    pub cr1: u32,
    pub cr2: u32,
    pub cr3: u32,
    pub sr: RtcSrFlags,
    pub prer: RtcPrescaler,
    pub cnt: u32,           // Current subsecond counter
    pub div: u32,           // Fractional division (for subsecond)
    pub time: RtcTime,      // Calendar time
    pub alarm_a: RtcAlarm,
    pub alarm_b: RtcAlarm,
    pub waktr: u16,         // Wakeup timer reload value
    pub wakeup_counter: u16,// Current wakeup countdown
    /// Total simulated seconds elapsed (internal).
    pub elapsed_seconds: u64,
}

impl RtcPeripheral {
    pub fn new(base: u32) -> Self {
        Self {
            base,
            cr1: 0,
            cr2: 0,
            cr3: 0,
            sr: RtcSrFlags::default(),
            prer: RtcPrescaler::default(),
            cnt: 0,
            div: 0,
            time: RtcTime::default(),
            alarm_a: RtcAlarm::default(),
            alarm_b: RtcAlarm::default(),
            waktr: 0,
            wakeup_counter: 0,
            elapsed_seconds: 0,
        }
    }

    /// Set initial time (convenient for test fixtures).
    pub fn set_initial_time(&mut self, time: RtcTime) {
        self.time = time;
    }

    pub fn read_reg(&self, offset: u32) -> Option<u32> {
        let val = match offset {
            0x00 => self.cr1,
            0x04 => self.cr2,
            0x08 => self.cr3,
            0x0C => self.sr.encode(),
            0x10 => {
                ((self.prer.prediv_a as u32) << 16) | (self.prer.prediv_b as u32)
            }
            0x14 => self.div,
            0x18 => self.cnt >> 16,  // High part of counter
            0x1C => self.cnt & 0xFFFF, // Low part
            0x20 => {
                // ALRMAR - encode alarm A
                let mut reg = 0u32;
                if self.alarm_a.mask_sec { reg |= 1 << 31; }
                if self.alarm_a.mask_min { reg |= 1 << 24; }
                if self.alarm_a.mask_hour { reg |= 1 << 16; }
                if self.alarm_a.mask_day { reg |= 1 << 13; }
                if self.alarm_a.mask_month { reg |= 1 << 12; }
                if self.alarm_a.mask_wday { reg |= 1 << 11; }
                reg |= (self.alarm_a.target.sec as u32) << 0;
                reg |= (self.alarm_a.target.min as u32) << 8;
                reg |= (self.alarm_a.target.hour as u32) << 16;
                reg
            }
            0x24 => 0, // ALRMBR placeholder
            _ => return None,
        };
        Some(val)
    }

    pub fn write_reg(&mut self, offset: u32, value: u32) {
        match offset {
            0x00 => {
                // CR1 - control register 1
                self.cr1 = value;
                // Bit 7: OUTEN (output enable), Bit 6: BKP (backup),
                // Bit 5: ALRAE (alarm A enable), Bit 4: TSE (timestamp),
                // Bit 3: WOFE (wake-up disable), Bit 2: CALP (calib parab),
                // Bit 1: CALM (calib mask), Bit 0: CKSE (clock select)
                if value & (1 << 5) != 0 {
                    self.alarm_a.enabled = true;
                } else {
                    self.alarm_a.enabled = false;
                }
            }
            0x04 => {
                // CR2 - control register 2
                self.cr2 = value;
                // Bit 6: ALRAF (alarm A flag), Bit 5: TSOE (timestamp output enable),
                // Bit 4: TSE (timestamp event), Bit 3: WUTF (wakeup timer flag),
                // Bit 2: WSOE (wakeup output enable), Bit 1: RSF (registers synchronized)
                if value & (1 << 6) != 0 {
                    self.sr.alrf = true;
                }
                if value & (1 << 1) != 0 {
                    self.sr.rsf = true;
                }
            }
            0x0C => {
                // SR - writing clears flags
                self.sr = RtcSrFlags {
                    rsf: true,
                    calff: false,
                    alrf: false,
                    wf: false,
                    initf: value & 1 != 0, // bit 0 = INIT
                };
            }
            0x10 => {
                // PRER - prescaler
                self.prer = RtcPrescaler {
                    prediv_a: ((value >> 16) & 0x7F) as u16,
                    prediv_b: (value & 0x1FFF) as u16,
                };
            }
            0x18 => {
                // CNTH - upper half of counter (subsecond fraction)
                self.cnt = (value << 16) | (self.cnt & 0xFFFF);
            }
            0x1C => {
                // CNTL - lower half of counter (seconds)
                self.cnt = (self.cnt & 0xFFFF_0000) | (value & 0xFFFF);
            }
            0x20 => {
                // ALRMAR - configure alarm A
                self.alarm_a.mask_sec = (value >> 31 & 1) != 0;
                self.alarm_a.mask_min = (value >> 24 & 1) != 0;
                self.alarm_a.mask_hour = (value >> 16 & 1) != 0;
                self.alarm_a.mask_day = (value >> 13 & 1) != 0;
                self.alarm_a.mask_month = (value >> 12 & 1) != 0;
                self.alarm_a.mask_wday = (value >> 11 & 1) != 0;
                self.alarm_a.target.sec = (value & 0x7F) as u8;
                self.alarm_a.target.min = ((value >> 8) & 0x7F) as u8;
                self.alarm_a.target.hour = ((value >> 16) & 0x3F) as u8;
                self.alarm_a.enabled = true;
            }
            _ => {}
        }
    }

    /// Tick the RTC by one second. Call this once per simulated second.
    pub fn tick_second(&mut self) {
        self.elapsed_seconds += 1;
        
        // Update time
// capture not needed
        
        self.time.sec += 1;
        if self.time.sec >= 60 {
            self.time.sec = 0;
            self.time.min += 1;
            
            if self.time.min >= 60 {
                self.time.min = 0;
                self.time.hour += 1;
                
                if self.time.hour >= 24 {
                    self.time.hour = 0;
                    self.time.day += 1;
                    let new_wday = (self.time.wday % 7u8) + 1;
                    self.time.wday = if new_wday > 7 { 1 } else { new_wday };
                    
                    if self.time.day > 28 {
                        self.time.day = 1;
                        self.time.month += 1;
                        
                        if self.time.month > 12 {
                            self.time.month = 1;
                            self.time.year += 1;
                        }
                    }
                }
            }
        }
        
        // Check alarm A - capture values first to avoid borrow issues
        if self.alarm_a.enabled {
            let sec_match = !self.alarm_a.mask_sec || self.alarm_a.target.sec == self.time.sec;
            let min_match = !self.alarm_a.mask_min || self.alarm_a.target.min == self.time.min;
            let hour_match = !self.alarm_a.mask_hour || self.alarm_a.target.hour == self.time.hour;
            if sec_match && min_match && hour_match {
                self.sr.alrf = true;
            }
        }
    }


    pub fn contains(&self, address: u32) -> bool {
        address >= self.base && address < self.base + 0x28
    }

    pub fn offset(&self, address: u32) -> u32 {
        address.wrapping_sub(self.base)
    }
}

impl RtcSrFlags {
    pub fn encode(&self) -> u32 {
        let mut reg = 0u32;
        if self.rsf { reg |= 1 << 3; }
        if self.calff { reg |= 1 << 2; }
        if self.alrf { reg |= 1 << 1; }
        if self.wf { reg |= 1 << 0; }
        if self.initf { reg |= 1 << 7; }
        reg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtc_new_has_default_state() {
        let rtc = RtcPeripheral::new(0x4000_2800);
        assert_eq!(rtc.time.sec, 0);
        assert_eq!(rtc.time.min, 0);
        assert_eq!(rtc.time.hour, 0);
        assert!(rtc.sr.rsf);
    }

    #[test]
    fn rtc_tick_advances_seconds() {
        let mut rtc = RtcPeripheral::new(0x4000_2800);
        rtc.set_initial_time(RtcTime { sec: 30, ..Default::default() });
        
        rtc.tick_second();
        assert_eq!(rtc.time.sec, 31);
        
        rtc.tick_second();
        assert_eq!(rtc.time.sec, 32);
    }

    #[test]
    fn rtc_tick_advances_minutes() {
        let mut rtc = RtcPeripheral::new(0x4000_2800);
        rtc.set_initial_time(RtcTime { sec: 59, min: 30, ..Default::default() });
        
        rtc.tick_second();
        assert_eq!(rtc.time.sec, 0);
        assert_eq!(rtc.time.min, 31);
    }

    #[test]
    fn rtc_tick_advances_hours() {
        let mut rtc = RtcPeripheral::new(0x4000_2800);
        rtc.set_initial_time(RtcTime { sec: 59, min: 59, hour: 23, ..Default::default() });
        
        rtc.tick_second();
        assert_eq!(rtc.time.sec, 0);
        assert_eq!(rtc.time.min, 0);
        assert_eq!(rtc.time.hour, 0);
    }

    #[test]
    fn rtc_read_prescaler() {
        let rtc = RtcPeripheral::new(0x4000_2800);
        let prer = rtc.read_reg(0x10).unwrap();
        let prediv_a = (prer >> 16) & 0x7F;
        let prediv_b = prer & 0x1FFF;
        assert_eq!(prediv_a, 127);
        assert_eq!(prediv_b, 255);
    }

    #[test]
    fn rtc_write_prescaler() {
        let mut rtc = RtcPeripheral::new(0x4000_2800);
        rtc.write_reg(0x10, 0x007F_00FF); // prediv_a=127, prediv_b=255
        
        assert_eq!(rtc.prer.prediv_a, 127);
        assert_eq!(rtc.prer.prediv_b, 255);
    }

    #[test]
    fn rtc_alarm_matches_and_clears() {
        let mut rtc = RtcPeripheral::new(0x4000_2800);
        
        // Configure alarm at 12:00:30
        rtc.alarm_a.target = RtcTime { sec: 30, min: 0, hour: 12, ..Default::default() };
        rtc.alarm_a.enabled = true;
        
        // Simulate reaching that time
        rtc.set_initial_time(RtcTime { sec: 29, min: 0, hour: 12, ..Default::default() });
        rtc.tick_second();
        
        assert!(rtc.sr.alrf);
    }

    #[test]
    fn rtc_contains_range() {
        let rtc = RtcPeripheral::new(0x4000_2800);
        assert!(rtc.contains(0x4000_2800));
        assert!(rtc.contains(0x4000_2824)); // ALRMBR
        assert!(!rtc.contains(0x4000_2828));
    }

    #[test]
    fn rtc_offset_calculation() {
        let rtc = RtcPeripheral::new(0x4000_2800);
        assert_eq!(rtc.offset(0x4000_2800), 0);
        assert_eq!(rtc.offset(0x4000_2810), 0x10);
        assert_eq!(rtc.offset(0x4000_2820), 0x20);
    }
}
