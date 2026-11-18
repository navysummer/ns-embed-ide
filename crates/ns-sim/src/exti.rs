//! Virtual External Interrupt (EXTI) controller for ns-sim.
//!
//! Models the STM32-style EXTI controller that allows GPIO pins to trigger
//! interrupts on rising/falling/high edges.

use serde::Serialize;

/// EXTI trigger edge type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ExtiEdge {
    Rising,
    Falling,
    Both,
}

impl Default for ExtiEdge {
    fn default() -> Self {
        Self::Falling
    }
}

/// A single EXTI line configuration.
#[derive(Debug, Clone, Serialize)]
pub struct ExtiLine {
    /// Pin number (0-15) on the GPIO port.
    pub pin: u8,
    /// Trigger edge sensitivity.
    pub trigger: ExtiEdge,
    /// Whether the interrupt is enabled.
    pub enabled: bool,
}

impl Default for ExtiLine {
    fn default() -> Self {
        Self {
            pin: 0,
            trigger: ExtiEdge::default(),
            enabled: false,
        }
    }
}

impl ExtiLine {
    /// Check if this EXTI line should trigger on the given edge.
    pub fn triggers_on(&self, edge: &ExtiEdge) -> bool {
        if !self.enabled {
            return false;
        }
        match self.trigger {
            ExtiEdge::Rising => matches!(edge, ExtiEdge::Rising),
            ExtiEdge::Falling => matches!(edge, ExtiEdge::Falling),
            ExtiEdge::Both => true,
        }
    }
}

/// External Interrupt/Event controller.
#[derive(Debug, Clone, Serialize)]
pub struct ExtiController {
    /// Base address for register mapping (typically 0x4001_0000 for STM32).
    pub base: u32,
    /// EXTI lines 0-15.
    pub lines: Vec<ExtiLine>,
    /// Current state of each GPIO input pin (for edge detection).
    pub pin_states: [bool; 16],
}

impl ExtiController {
    pub fn new(base: u32) -> Self {
        Self {
            base,
            lines: (0..16)
                .map(|_| ExtiLine::default())
                .collect(),
            pin_states: [false; 16],
        }
    }

    /// Configure an EXTI line for a specific pin.
    pub fn configure_line(&mut self, pin: usize, trigger: ExtiEdge, enabled: bool) {
        if pin < self.lines.len() {
            self.lines[pin] = ExtiLine {
                pin: pin as u8,
                trigger,
                enabled,
            };
        }
    }

    /// Sample the current GPIO state and detect edge transitions.
    /// Returns any triggered IRQ line numbers.
    pub fn sample(&mut self, new_states: [bool; 16]) -> Vec<usize> {
        let mut triggered = Vec::new();
        for i in 0..16 {
            let old_state = self.pin_states[i];
            let new_state = new_states[i];
            if old_state == new_state {
                continue; // No change
            }
            let edge = if new_state {
                ExtiEdge::Rising
            } else {
                ExtiEdge::Falling
            };
            if self.lines[i].triggers_on(&edge) {
                triggered.push(i);
            }
            self.pin_states[i] = new_state;
        }
        triggered
    }

    /// Get register values for memory-mapped access.
    pub fn read_reg(&self, offset: u32) -> Option<u32> {
        match offset {
            0x00 => {
                // IMR: Interrupt Mask Register
                let mut mask = 0u32;
                for (i, line) in self.lines.iter().enumerate() {
                    if line.enabled {
                        mask |= 1 << i;
                    }
                }
                Some(mask)
            }
            0x04 => {
                // EMR: Event Mask Register
                let mut mask = 0u32;
                for line in &self.lines {
                    if line.enabled {
                        mask |= 1 << line.pin;
                    }
                }
                Some(mask)
            }
            0x08 => {
                // RTSR: Rising Trigger Selection Register
                let mut reg = 0u32;
                for line in &self.lines {
                    if matches!(line.trigger, ExtiEdge::Rising | ExtiEdge::Both) {
                        reg |= 1 << line.pin;
                    }
                }
                Some(reg)
            }
            0x0C => {
                // FTSR: Falling Trigger Selection Register
                let mut reg = 0u32;
                for line in &self.lines {
                    if matches!(line.trigger, ExtiEdge::Falling | ExtiEdge::Both) {
                        reg |= 1 << line.pin;
                    }
                }
                Some(reg)
            }
            0x10 => {
                // PR: Pending Register (write 1 to clear)
                let mut reg = 0u32;
                for line in &self.lines {
                    if line.enabled && self.pin_states[line.pin as usize] {
                        reg |= 1 << line.pin;
                    }
                }
                Some(reg)
            }
            _ => None,
        }
    }

    /// Set register values for memory-mapped access.
    pub fn write_reg(&mut self, offset: u32, value: u32) {
        match offset {
            0x00 => {
                // IMR write
                for i in 0..16 {
                    self.lines[i].enabled = (value >> i) & 1 != 0;
                }
            }
            0x08 => {
                // RTSR write
                for i in 0..16 {
                    if (value >> i) & 1 != 0 {
                        self.configure_line(i, ExtiEdge::Rising, self.lines[i].enabled);
                    } else if self.lines[i].trigger == ExtiEdge::Rising {
                        self.configure_line(i, ExtiEdge::Falling, self.lines[i].enabled);
                    }
                }
            }
            0x0C => {
                // FTSR write
                for i in 0..16 {
                    if (value >> i) & 1 != 0 {
                        self.configure_line(i, ExtiEdge::Falling, self.lines[i].enabled);
                    } else if self.lines[i].trigger == ExtiEdge::Falling {
                        self.configure_line(i, ExtiEdge::Rising, self.lines[i].enabled);
                    }
                }
            }
            0x10 => {
                // PR write: clear pending flags by writing 1
                for i in 0..16 {
                    if (value >> i) & 1 != 0 {
                        self.pin_states[i] = false;
                    }
                }
            }
            _ => {}
        }
    }

    pub fn contains(&self, address: u32) -> bool {
        address >= self.base && address < self.base + 0x14
    }

    pub fn offset(&self, address: u32) -> u32 {
        address.wrapping_sub(self.base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exti_new_has_defaults() {
        let exti = ExtiController::new(0x4001_0000);
        assert_eq!(exti.lines.len(), 16);
        assert!(!exti.lines[0].enabled);
        assert_eq!(exti.lines[0].trigger, ExtiEdge::Falling);
    }

    #[test]
    fn exti_configure_line() {
        let mut exti = ExtiController::new(0x4001_0000);
        exti.configure_line(0, ExtiEdge::Rising, true);
        assert!(exti.lines[0].enabled);
        assert_eq!(exti.lines[0].trigger, ExtiEdge::Rising);
    }

    #[test]
    fn exti_detects_rising_edge() {
        let mut exti = ExtiController::new(0x4001_0000);
        exti.configure_line(5, ExtiEdge::Rising, true);
        // Initial state: pin 5 low
        exti.pin_states[5] = false;
        
        // Simulate rising edge on pin 5
        let triggered = exti.sample([false, false, false, false, false, true, false, false, false, false, false, false, false, false, false, false]);
        assert_eq!(triggered, vec![5]);
    }

    #[test]
    fn exti_detects_both_edges() {
        let mut exti = ExtiController::new(0x4001_0000);
        exti.configure_line(3, ExtiEdge::Both, true);
        
        // Rising edge
        let t1 = exti.sample([false, false, false, true, false, false, false, false, false, false, false, false, false, false, false, false]);
        assert_eq!(t1, vec![3]);
        
        // Falling edge
        let t2 = exti.sample([false, false, false, false, false, false, false, false, false, false, false, false, false, false, false, false]);
        assert_eq!(t2, vec![3]);
    }

    #[test]
    fn exti_registers_read_write() {
        let mut exti = ExtiController::new(0x4001_0000);
        exti.configure_line(0, ExtiEdge::Rising, true);
        exti.configure_line(2, ExtiEdge::Falling, true);
        
        // Read IMR
        assert_eq!(exti.read_reg(0x00), Some(0b101));
        
        // Write RTSR
        exti.write_reg(0x08, 0xFF);
        assert_eq!(exti.read_reg(0x08), Some(0xFF));
    }
}
