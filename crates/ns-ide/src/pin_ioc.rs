//! Pin assignments read out of a STM32CubeMX `.ioc` file.
//!
//! CubeMX stores one key per configured pin and per field, e.g.
//! `Signal.PA9=USART1_TX`, `Mode.PA1=GPIO`, `Pull.PA1=PUp` (older files use
//! `PA9.Signal0=…` / `PA9.Mode=…`). Only the pin-level data is extracted here.
//! Only the pin-level data is extracted here: peripheral clocks and instance
//! settings belong to the project import path (`ns_project::import`).

/// One pin assignment taken from a `.ioc` document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IocPinEntry {
    pub port: char,
    pub pin: usize,
    /// Raw CubeMX value: either a `Signal0` name (`USART1_TX`, `TIM2_CH3`) or a
    /// GPIO `Mode` (`GPIO_Output`, `GPIO_Input_Pu`, `GPIO_Input_Pd`).
    pub raw: String,
}

/// Everything the pin map needs from one `.ioc` document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IocPinMap {
    /// `Mcu.PartNumber` / `Mcu.Name`, verbatim; used to reject a foreign MCU.
    pub mcu: Option<String>,
    /// Assignments in file order. When CubeMX repeats a pin (GPIO `Mode` plus
    /// `Signal0=GPIO`), the more informative value wins.
    pub entries: Vec<IocPinEntry>,
}

/// The per-pin fields CubeMX writes, either as `Mode.PA9` / `Signal.PA9`
/// (CubeMX) or as `PA9.Mode` / `PA9.Signal0` (older compatible layouts).
#[derive(Default)]
struct PinFields {
    signal: Option<String>,
    mode: Option<String>,
    signal_mode: Option<String>,
    pull: Option<String>,
}

impl PinFields {
    /// Record one field; false when the name is not pin data at all.
    fn set(&mut self, field: &str, value: String) -> bool {
        let slot = match field {
            "Signal" => &mut self.signal,
            "Mode" => &mut self.mode,
            "SignalMode" => &mut self.signal_mode,
            "Pull" => &mut self.pull,
            _ => return false,
        };
        *slot = Some(value);
        true
    }

    /// The value the pin map should use: a peripheral signal beats the port
    /// level mode, a GPIO mode beats a bare `Signal=GPIO`, and `SignalMode`
    /// plus `Pull` fill in what neither of them says.
    fn resolve(self) -> String {
        let is_gpio_value = |value: &str| {
            matches!(
                value,
                "GPIO" | "GPIO_Output" | "GPIO_Input" | "GPIO_Input_Pu" | "GPIO_Input_Pd"
            )
        };
        let base = self
            .signal
            .as_deref()
            .filter(|value| !is_gpio_value(value))
            .or_else(|| self.mode.as_deref().filter(|value| *value != "GPIO"))
            .map(|value| value.to_string())
            .unwrap_or_else(|| gpio_from_signal_mode(&self.signal_mode).to_string());
        if base == "GPIO_Input" {
            match self.pull.as_deref() {
                Some("PUp") => "GPIO_Input_Pu".to_string(),
                Some("PDown") => "GPIO_Input_Pd".to_string(),
                _ => base,
            }
        } else {
            base
        }
    }
}

/// What a bare GPIO `SignalMode` line says about the pin direction.
fn gpio_from_signal_mode(signal_mode: &Option<String>) -> &'static str {
    match signal_mode.as_deref() {
        Some("RegularOutput" | "UserRegularOutput" | "MapReuse") => "GPIO_Output",
        Some("Input" | "Analog" | "UserAnalog") => "GPIO_Input",
        _ => "GPIO",
    }
}

/// Parse pin assignments out of `.ioc` text. Lines that are not pin data —
/// `Mcu.IP_*`, `ProjectManager.*`, `Locked.PA9`, clock and peripheral settings
/// — pass through ignored.
pub fn parse(text: &str) -> IocPinMap {
    let mut map = IocPinMap::default();
    let mut found: Vec<(char, usize, PinFields)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with(';')
            || line.starts_with('[')
        {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if key.is_empty() || value.is_empty() {
            continue;
        }
        if matches!(key, "Mcu.PartNumber" | "Mcu.Name" | "Mcu.Pin" | "McuName") {
            if map.mcu.as_deref().is_none_or(|current| current == "None") {
                map.mcu = Some(value.to_string());
            }
            continue;
        }
        let Some((prefix, suffix)) = key.split_once('.') else {
            continue;
        };
        // `Mode.PA9=GPIO`: CubeMX names the field before the pin.
        if is_pin_field(prefix) {
            let Some((port, pin)) = split_pin(suffix) else {
                continue;
            };
            record(&mut found, port, pin, prefix, value);
            continue;
        }
        // `PA9.Signal0=USART1_TX`: the compatible layout names the pin first.
        let Some((port, pin)) = split_pin(prefix) else {
            continue;
        };
        let field = if suffix == "Mode" {
            "Mode"
        } else if suffix.starts_with("Signal")
            && suffix["Signal".len()..].chars().all(|c| c.is_ascii_digit())
        {
            "Signal"
        } else {
            continue;
        };
        record(&mut found, port, pin, field, value);
    }

    map.entries = found
        .into_iter()
        .map(|(port, pin, fields)| IocPinEntry {
            port,
            pin,
            raw: fields.resolve(),
        })
        // A bare `GPIO` (or a `Pull`-only line) carries no assignment.
        .filter(|entry| entry.raw != "GPIO")
        .collect();
    map
}

/// CubeMX writes `SignalMode.PA9=AS` and `Locked.PA9=0` beside the fields
/// above; only these four names carry pin data.
fn is_pin_field(field: &str) -> bool {
    matches!(field, "Signal" | "Mode" | "SignalMode" | "Pull")
}

fn record(
    found: &mut Vec<(char, usize, PinFields)>,
    port: char,
    pin: usize,
    field: &str,
    value: &str,
) {
    match found
        .iter()
        .position(|(known_port, known_pin, _)| *known_port == port && *known_pin == pin)
    {
        Some(index) => {
            found[index].2.set(field, value.to_string());
        }
        None => {
            let mut fields = PinFields::default();
            fields.set(field, value.to_string());
            found.push((port, pin, fields));
        }
    }
}

/// CubeMX part naming for the targets the built-in AF tables cover:
/// `(McuType, McuPackage, McuName)`.
pub fn mcu_identity(target: &str) -> Option<(&'static str, &'static str, &'static str)> {
    match target.to_ascii_lowercase().as_str() {
        "stm32f407" | "stm32f407vg" => Some(("STM32F407", "LQFP100", "STM32F407VGTx")),
        "stm32f103c6" => Some(("STM32F103", "LQFP48", "STM32F103C6Tx")),
        "stm32f103c8" => Some(("STM32F103", "LQFP48", "STM32F103C8Tx")),
        "stm32f103cb" => Some(("STM32F103", "LQFP48", "STM32F103CBTx")),
        _ => None,
    }
}

/// The pin name CubeMX uses in `Mode.PA9` and `pin_map.json` (`PA9`, `PC13`).
pub fn pin_name(port: char, pin: usize) -> String {
    format!("P{port}{pin}")
}

/// Split a `.ioc` pin prefix such as `PA9` into its port letter and number.
/// CubeMX writes the `P` for GPIO explicitly, so it is skipped; anything else
/// (`Mcu`, `Project`, ...`H1`) is not a pin of the built-in AF tables.
fn split_pin(prefix: &str) -> Option<(char, usize)> {
    let rest = prefix.strip_prefix('P')?;
    let port = rest.chars().next()?;
    if !matches!(port, 'A'..='E' | 'I') {
        return None;
    }
    let digits = &rest[port.len_utf8()..];
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let pin: usize = digits.parse().ok()?;
    (pin <= 15).then_some((port, pin))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A representative CubeMX document: GPIO modes, alternate functions, the
    /// Cortex-M debug signals, an unmodelled ethernet pin and noise that must be
    /// ignored.
    const F407_IOC: &str = r#"# generated by STM32CubeMX
Mcu.IP_TIM2=TIM2
Mcu.IP_I2C1=I2C1
Mcu.PartNumber=STM32F407VGTx
Mcu.UserName=Target
PA9.Signal0=USART1_TX
PA10.Signal0=USART1_RX
PB6.Signal0=I2C1_SCL
PB7.Signal0=I2C1_SDA
PA0.Mode=GPIO_Output
PA0.Signal0=GPIO
PA1.Mode=GPIO_Input_Pu
PA1.Signal0=GPIO
PC14.Signal0=GPIO
PA5.Signal0=TIM2_CH1_ETR
PA6.Signal0=TIM2_CH3
PA7.Signal0=TIM2_CH4
PA13.Signal0=SYS_JTMS-SWO
PA14.Signal0=SYS_JTCK-SWCLK
PB12.Signal0=USB_OTG_FS_DM
PD9.Signal0=ETH_MII_CRS
PC13.Mode=GPIO_Input_Pd
PC13.Signal0=GPIO
PA8.Signal0=TIM1_CH1
PB14.Signal0=TIM12_CH1
PA0.Signal0=TIM2_CH1
PA1.Lock=2
ProjectManager.Add2TableClickCallback=0
"#;

    #[test]
    fn parses_pin_signals_gpio_modes_and_mcu_name() {
        let map = parse(F407_IOC);
        assert_eq!(map.mcu.as_deref(), Some("STM32F407VGTx"));
        assert_eq!(
            map.entries.len(),
            16,
            "peripheral/IP/lock keys must be ignored"
        );

        let lookup = |port: char, pin: usize| -> Option<&str> {
            map.entries
                .iter()
                .find(|entry| entry.port == port && entry.pin == pin)
                .map(|entry| entry.raw.as_str())
        };
        assert_eq!(lookup('A', 9), Some("USART1_TX"));
        assert_eq!(lookup('B', 7), Some("I2C1_SDA"));
        assert_eq!(lookup('A', 6), Some("TIM2_CH3"));
        assert_eq!(lookup('A', 13), Some("SYS_JTMS-SWO"));
        assert_eq!(lookup('B', 12), Some("USB_OTG_FS_DM"));
        assert_eq!(lookup('D', 9), Some("ETH_MII_CRS"));
    }

    #[test]
    fn gpio_mode_wins_over_bare_gpio_signal() {
        let map = parse(F407_IOC);
        // `PA0.Mode=GPIO_Output` precedes `PA0.Signal0=GPIO`, and the later
        // `PA0.Signal0=TIM2_CH1` overrides both.
        assert_eq!(find_raw(&map, 'A', 0), Some("TIM2_CH1"));
        assert_eq!(find_raw(&map, 'A', 1), Some("GPIO_Input_Pu"));
        assert_eq!(find_raw(&map, 'C', 13), Some("GPIO_Input_Pd"));
        // A bare `Signal0=GPIO` without a Mode line is not an assignment and is
        // not reported as one.
        assert_eq!(find_raw(&map, 'C', 14), None);
    }

    /// The layout CubeMX actually writes: the field name comes before the pin
    /// (`Mode.PA9`), and `SignalMode`/`Pull` carry direction and pull-up.
    const NATIVE_F407_IOC: &str = r#"#MicroXplorer Configuration settings - do not modify
McuType=STM32F407
McuFamily=STM32F4
McuPackage=LQFP100
McuName=STM32F407VGTx
McuDie=STM32F407
Mcu.IP_TIM2=TIM2
Mcu.Pin=STM32F407VGTx
Mode.PA9=GPIO
Signal.PA9=USART1_TX
SignalMode.PA9=AS
Locked.PA9=0
Unlock.PA9=0
Mode.PB6=GPIO
Signal.PB6=I2C1_SCL
SignalMode.PB6=AS
Mode.PA1=GPIO
Signal.PA1=GPIO
SignalMode.PA1=Input
Pull.PA1=PUp
Mode.PA5=GPIO
Signal.PA5=GPIO
SignalMode.PA5=RegularOutput
Mode.PA13=GPIO
Signal.PA13=SYS_JTMS-SWO
SignalMode.PA13=AS
Pull.PA0=NoPull
ProjectManager.KeepUserCode=0
Mcu.UserName=Target
"#;

    #[test]
    fn parses_native_cubemx_pin_lines() {
        let map = parse(NATIVE_F407_IOC);
        assert_eq!(map.mcu.as_deref(), Some("STM32F407VGTx"));
        assert_eq!(
            map.entries.len(),
            5,
            "Locked/Unlock and Pull-only lines must not create pins"
        );
        assert_eq!(find_raw(&map, 'A', 9), Some("USART1_TX"));
        assert_eq!(find_raw(&map, 'B', 6), Some("I2C1_SCL"));
        assert_eq!(
            find_raw(&map, 'A', 1),
            Some("GPIO_Input_Pu"),
            "SignalMode=Input plus Pull=PUp"
        );
        assert_eq!(
            find_raw(&map, 'A', 5),
            Some("GPIO_Output"),
            "SignalMode=RegularOutput"
        );
        assert_eq!(find_raw(&map, 'A', 13), Some("SYS_JTMS-SWO"));
    }

    #[test]
    fn rejects_non_pin_prefixes_and_out_of_range_pins() {
        let map = parse(
            "H1.Signal0=TIM1_CH1\nPA16.Signal0=TIM2_CH1\nMcu.Name=STM32F103C8Tx\nPA4.Signal0=SPI1_NSS\n",
        );
        assert_eq!(map.mcu.as_deref(), Some("STM32F103C8Tx"));
        assert_eq!(map.entries.len(), 1);
        assert_eq!(map.entries[0].port, 'A');
        assert_eq!(map.entries[0].pin, 4);
    }

    fn find_raw(map: &IocPinMap, port: char, pin: usize) -> Option<&str> {
        map.entries
            .iter()
            .find(|entry| entry.port == port && entry.pin == pin)
            .map(|entry| entry.raw.as_str())
    }
}
