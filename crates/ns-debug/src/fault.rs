//! ARM Cortex-M fault register decoder (HardFault / MemManage / BusFault / UsageFault).
//!
//! Pure, side-effect-free decoding of a [`FaultSnapshot`] into a human-readable
//! [`FaultReport`]. No DAP session, no GPUI window and no hardware access is
//! required: the snapshot can be captured live by the IDE or supplied by an
//! external caller (e.g. the MCP `decode_hardfault` tool).
//!
//! Register addresses follow ARMv7-M/ARMv8-M:
//! - `CFSR`  0xE000_ED28 (MMFSR bits 7:0, BFSR 15:8, UFSR 31:16)
//! - `HFSR`  0xE000_ED2C
//! - `MMFAR` 0xE000_ED34
//! - `BFAR`  0xE000_ED38
//! - `AFSR`  0xE000_ED3C

use serde::{Deserialize, Serialize};

/// Raw fault-scene snapshot. Serialization support lets the IDE export it and
/// an MCP caller feed the exact same JSON back in for decoding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaultSnapshot {
    /// Configurable Fault Status Register.
    pub cfsr: u32,
    /// HardFault Status Register.
    pub hfsr: u32,
    /// Memory Management Fault Address Register (`None` if not readable).
    pub mmfar: Option<u32>,
    /// Bus Fault Address Register (`None` if not readable).
    pub bfar: Option<u32>,
    /// Auxiliary Fault Status Register.
    pub afsr: Option<u32>,
    /// Exception stack frame at SP: R0,R1,R2,R3,R12,LR,PC,xPSR.
    pub stacked: [u32; 8],
    /// EXC_RETURN value in LR at fault entry (optional).
    pub lr_exc_return: Option<u32>,
}

/// Severity of one decoded bit, used by the UI to color the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FaultSeverity {
    /// A fault cause — directly explains why the exception fired.
    Cause,
    /// Context information (e.g. forced escalation, vector table error).
    Context,
    /// A "valid" flag making MMFAR/BFAR meaningful.
    ValidFlag,
}

impl FaultSeverity {
    pub fn label(self) -> &'static str {
        match self {
            FaultSeverity::Cause => "cause",
            FaultSeverity::Context => "context",
            FaultSeverity::ValidFlag => "valid",
        }
    }
}

/// One set bit decoded into a named, explained field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaultField {
    /// Register that contains the bit (`CFSR` / `HFSR`).
    pub register: &'static str,
    /// Bit number within the register.
    pub bit: u8,
    /// Field mnemonic (e.g. `PRECISERR`).
    pub name: &'static str,
    /// Human-readable explanation in Chinese.
    pub description: &'static str,
    pub severity: FaultSeverity,
}

/// Final diagnosis produced from a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaultReport {
    /// Primary fault category: `MemManage` / `BusFault` / `UsageFault` / `HardFault` / `Unknown`.
    pub fault_type: &'static str,
    /// True when HFSR.FORCED is set (escalated from a configurable fault).
    pub is_forced: bool,
    /// Faulting instruction address from the stacked PC (`stacked[6]`).
    pub fault_pc: u32,
    /// Stacked link register (`stacked[5]`), useful for call-site recovery.
    pub fault_lr: u32,
    /// Stacked program status register (`stacked[7]`).
    pub xpsr: u32,
    /// All set bits with their explanations.
    pub fields: Vec<FaultField>,
    /// Ordered, human-readable conclusions (causes first, then context).
    pub summary: Vec<String>,
    /// Interpreted EXC_RETURN (thread/handler mode, FPU frame, Thumb state).
    pub exc_return: Option<String>,
    /// Whether MMFAR is valid and should be trusted.
    pub mmfar_valid: bool,
    /// Whether BFAR is valid and should be trusted.
    pub bfar_valid: bool,
}

struct BitDef {
    register: &'static str,
    bit: u8,
    name: &'static str,
    description: &'static str,
    severity: FaultSeverity,
}

const CFSR_BITS: &[BitDef] = &[
    // MemManage Status Register (bits 7:0)
    BitDef {
        register: "CFSR",
        bit: 0,
        name: "IACCVIOL",
        description: "指令访问违例：尝试执行不允许区域的代码",
        severity: FaultSeverity::Cause,
    },
    BitDef {
        register: "CFSR",
        bit: 1,
        name: "DACCVIOL",
        description: "数据访问违例：加载/存储到不允许的区域",
        severity: FaultSeverity::Cause,
    },
    BitDef {
        register: "CFSR",
        bit: 3,
        name: "MUNSTKERR",
        description: "异常返回出栈时发生 MemManage 错误",
        severity: FaultSeverity::Cause,
    },
    BitDef {
        register: "CFSR",
        bit: 4,
        name: "MSTKERR",
        description: "异常入栈时发生 MemManage 错误（常见栈溢出）",
        severity: FaultSeverity::Cause,
    },
    BitDef {
        register: "CFSR",
        bit: 5,
        name: "MLSPERR",
        description: "FPU 惰性栈保存时发生 MemManage 错误",
        severity: FaultSeverity::Cause,
    },
    BitDef {
        register: "CFSR",
        bit: 7,
        name: "MMARVALID",
        description: "MMFAR 有效：地址寄存器给出故障地址",
        severity: FaultSeverity::ValidFlag,
    },
    // Bus Fault Status Register (bits 15:8)
    BitDef {
        register: "CFSR",
        bit: 8,
        name: "IBUSERR",
        description: "指令总线错误：取指时总线错误（PC 不精确）",
        severity: FaultSeverity::Cause,
    },
    BitDef {
        register: "CFSR",
        bit: 9,
        name: "PRECISERR",
        description: "精确总线错误：PC 指向故障指令，可反汇编定位",
        severity: FaultSeverity::Cause,
    },
    BitDef {
        register: "CFSR",
        bit: 10,
        name: "IMPRECISERR",
        description: "非精确总线错误：PC 不可靠，故障指令可能在此前",
        severity: FaultSeverity::Cause,
    },
    BitDef {
        register: "CFSR",
        bit: 11,
        name: "UNSTKERR",
        description: "异常返回出栈时发生总线错误",
        severity: FaultSeverity::Cause,
    },
    BitDef {
        register: "CFSR",
        bit: 12,
        name: "STKERR",
        description: "异常入栈时发生总线错误（常见栈越界）",
        severity: FaultSeverity::Cause,
    },
    BitDef {
        register: "CFSR",
        bit: 13,
        name: "LSPERR",
        description: "FPU 惰性栈保存时发生总线错误",
        severity: FaultSeverity::Cause,
    },
    BitDef {
        register: "CFSR",
        bit: 15,
        name: "BFARVALID",
        description: "BFAR 有效：地址寄存器给出故障地址",
        severity: FaultSeverity::ValidFlag,
    },
    // Usage Fault Status Register (bits 31:16)
    BitDef {
        register: "CFSR",
        bit: 16,
        name: "UNDEFINSTR",
        description: "执行了未定义指令",
        severity: FaultSeverity::Cause,
    },
    BitDef {
        register: "CFSR",
        bit: 17,
        name: "INVSTATE",
        description: "EPSR.T 位无效：非 Thumb 状态执行（Cortex-M 必须 Thumb）",
        severity: FaultSeverity::Cause,
    },
    BitDef {
        register: "CFSR",
        bit: 18,
        name: "INVPC",
        description: "非法的 PC 加载（EXC_RETURN 不匹配）",
        severity: FaultSeverity::Cause,
    },
    BitDef {
        register: "CFSR",
        bit: 19,
        name: "NOCP",
        description: "访问了不存在的协处理器",
        severity: FaultSeverity::Cause,
    },
    BitDef {
        register: "CFSR",
        bit: 24,
        name: "UNALIGNED",
        description: "非对齐内存访问",
        severity: FaultSeverity::Cause,
    },
    BitDef {
        register: "CFSR",
        bit: 25,
        name: "DIVBYZERO",
        description: "整数除以零",
        severity: FaultSeverity::Cause,
    },
    BitDef {
        register: "CFSR",
        bit: 26,
        name: "STKOF",
        description: "栈溢出（ARMv8.1-M）",
        severity: FaultSeverity::Cause,
    },
];

const HFSR_BITS: &[BitDef] = &[
    BitDef {
        register: "HFSR",
        bit: 1,
        name: "VECTTBL",
        description: "向量表读取失败：异常向量地址无效",
        severity: FaultSeverity::Context,
    },
    BitDef {
        register: "HFSR",
        bit: 30,
        name: "FORCED",
        description: "强制升级：MemManage/BusFault/UsageFault 被升级为 HardFault",
        severity: FaultSeverity::Context,
    },
    BitDef {
        register: "HFSR",
        bit: 31,
        name: "DEBUGEVT",
        description: "调试事件导致的 HardFault",
        severity: FaultSeverity::Context,
    },
];

fn is_set(value: u32, bit: u8) -> bool {
    bit < 32 && (value >> bit) & 1 == 1
}

fn decode_exc_return(lr: u32) -> String {
    let mode = if lr & 0x4 == 0 { "handler" } else { "thread" };
    let fpu = if lr & 0x10 == 0 {
        "FPU frame"
    } else {
        "basic frame"
    };
    let thumb = if lr & 0x10 == 0 && lr & 0x1 == 1 {
        "Thumb"
    } else {
        // On Cortex-M the stacked xPSR.T bit is the authoritative Thumb
        // indicator; EXC_RETURN bit 4 selects the stack frame type.
        "see xPSR"
    };
    format!("0x{:08X}: 返回 {} 模式, {}, {}", lr, mode, fpu, thumb)
}

/// Decode a snapshot into a report. Pure function: identical input always
/// produces identical output, with no IO or global state.
pub fn decode_fault(snapshot: &FaultSnapshot) -> FaultReport {
    let mut fields = Vec::new();
    for def in CFSR_BITS.iter().chain(HFSR_BITS.iter()) {
        let value = if def.register == "CFSR" {
            snapshot.cfsr
        } else {
            snapshot.hfsr
        };
        if is_set(value, def.bit) {
            fields.push(FaultField {
                register: def.register,
                bit: def.bit,
                name: def.name,
                description: def.description,
                severity: def.severity,
            });
        }
    }

    let has = |name: &str| fields.iter().any(|f| f.name == name);
    let mmfar_valid = has("MMARVALID") && snapshot.mmfar.is_some();
    let bfar_valid = has("BFARVALID") && snapshot.bfar.is_some();
    let is_forced = has("FORCED");

    let mem_cause = ["IACCVIOL", "DACCVIOL", "MUNSTKERR", "MSTKERR", "MLSPERR"]
        .iter()
        .any(|n| has(n));
    let bus_cause = [
        "IBUSERR",
        "PRECISERR",
        "IMPRECISERR",
        "UNSTKERR",
        "STKERR",
        "LSPERR",
    ]
    .iter()
    .any(|n| has(n));
    let usage_cause = [
        "UNDEFINSTR",
        "INVSTATE",
        "INVPC",
        "NOCP",
        "UNALIGNED",
        "DIVBYZERO",
        "STKOF",
    ]
    .iter()
    .any(|n| has(n));

    // A HardFault escalation flag (FORCED, or a vector-table/debug fault)
    // wins as the primary category even when it was triggered by a lower
    // priority MemManage/BusFault/UsageFault cause. The cause is still listed
    // in the field/summary so the engineer sees what escalated.
    let hardfault_flag = is_forced || has("VECTTBL") || has("DEBUGEVT");
    let fault_type = if hardfault_flag {
        "HardFault"
    } else {
        let mut categories = Vec::new();
        if mem_cause {
            categories.push("MemManage");
        }
        if bus_cause {
            categories.push("BusFault");
        }
        if usage_cause {
            categories.push("UsageFault");
        }
        match categories.len() {
            0 => "Unknown",
            1 => categories[0],
            _ => "Multiple",
        }
    };

    // Causes first, then flags, then context — mirrors how an engineer reads
    // a fault report (what broke before where it was recorded).
    let mut summary: Vec<String> = fields
        .iter()
        .filter(|f| f.severity == FaultSeverity::Cause)
        .map(|f| format!("[{}] {}: {}", f.register, f.name, f.description))
        .collect();
    if mmfar_valid {
        summary.push(format!(
            "MMFAR = 0x{:08X}（MemManage 故障地址）",
            snapshot.mmfar.unwrap()
        ));
    }
    if bfar_valid {
        summary.push(format!(
            "BFAR = 0x{:08X}（BusFault 故障地址）",
            snapshot.bfar.unwrap()
        ));
    }
    for f in fields
        .iter()
        .filter(|f| f.severity == FaultSeverity::Context)
    {
        summary.push(format!("[{}] {}: {}", f.register, f.name, f.description));
    }
    summary.push(format!(
        "故障 PC = 0x{:08X}，LR = 0x{:08X}，xPSR = 0x{:08X}（来自异常栈帧）",
        snapshot.stacked[6], snapshot.stacked[5], snapshot.stacked[7]
    ));

    FaultReport {
        fault_type,
        is_forced,
        fault_pc: snapshot.stacked[6],
        fault_lr: snapshot.stacked[5],
        xpsr: snapshot.stacked[7],
        fields,
        summary,
        exc_return: snapshot.lr_exc_return.map(decode_exc_return),
        mmfar_valid,
        bfar_valid,
    }
}

/// Return the 8 words of the stacked exception frame with their architectural
/// names, for UI presentation.
pub fn stack_frame_labels() -> [&'static str; 8] {
    ["R0", "R1", "R2", "R3", "R12", "LR", "PC", "xPSR"]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(cfsr: u32, hfsr: u32) -> FaultSnapshot {
        FaultSnapshot {
            cfsr,
            hfsr,
            stacked: [0, 0, 0, 0, 0, 0x0800_1235, 0x0800_2000, 0x2100_0000],
            ..Default::default()
        }
    }

    fn names(report: &FaultReport) -> Vec<&str> {
        report.fields.iter().map(|f| f.name).collect()
    }

    #[test]
    fn empty_snapshot_is_unknown() {
        let report = decode_fault(&FaultSnapshot::default());
        assert_eq!(report.fault_type, "Unknown");
        assert!(report.fields.is_empty());
        assert!(!report.is_forced);
        assert_eq!(report.fault_pc, 0);
        assert_eq!(report.summary.len(), 1); // PC line always present
    }

    #[test]
    fn memmanage_cause_is_decoded() {
        let report = decode_fault(&snap(1 << 1, 0)); // DACCVIOL
        assert_eq!(report.fault_type, "MemManage");
        assert_eq!(names(&report), vec!["DACCVIOL"]);
        assert!(report.summary[0].contains("数据访问违例"));
        assert_eq!(report.fault_pc, 0x0800_2000);
    }

    #[test]
    fn precise_busfault_includes_bfar_when_valid() {
        let mut s = snap((1 << 9) | (1 << 15), 0); // PRECISERR | BFARVALID
        s.bfar = Some(0x4001_0000);
        let report = decode_fault(&s);
        assert_eq!(report.fault_type, "BusFault");
        assert!(report.bfar_valid);
        assert!(report.summary.iter().any(|l| l.contains("0x40010000")));
        assert!(report.summary.iter().any(|l| l.contains("精确总线错误")));
    }

    #[test]
    fn bfar_without_valid_flag_is_not_trusted() {
        let mut s = snap(1 << 9, 0);
        s.bfar = Some(0x4001_0000);
        let report = decode_fault(&s);
        assert!(!report.bfar_valid);
        assert!(!report.summary.iter().any(|l| l.contains("0x40010000")));
    }

    #[test]
    fn usage_fault_divzero_and_undefined() {
        let report = decode_fault(&snap((1 << 16) | (1 << 25), 0));
        assert_eq!(report.fault_type, "UsageFault");
        let n = names(&report);
        assert!(n.contains(&"UNDEFINSTR"));
        assert!(n.contains(&"DIVBYZERO"));
    }

    #[test]
    fn forced_hardfault_is_flagged_and_contextualized() {
        let report = decode_fault(&snap(1 << 1, 1 << 30)); // DACCVIOL + FORCED
        assert_eq!(report.fault_type, "HardFault");
        assert_eq!(report.fault_type, "HardFault");
        assert!(report.is_forced);
        assert!(report.summary.iter().any(|l| l.contains("强制升级")));
        // Cause is listed before context.
        let cause_pos = report
            .summary
            .iter()
            .position(|l| l.contains("数据访问违例"))
            .unwrap();
        let ctx_pos = report
            .summary
            .iter()
            .position(|l| l.contains("强制升级"))
            .unwrap();
        assert!(cause_pos < ctx_pos);
    }

    #[test]
    fn vector_table_and_debug_faults_are_hardfault() {
        assert_eq!(decode_fault(&snap(0, 1 << 1)).fault_type, "HardFault");
        assert_eq!(decode_fault(&snap(0, 1 << 31)).fault_type, "HardFault");
    }

    #[test]
    fn multiple_simultaneous_causes_are_reported() {
        let report = decode_fault(&snap((1 << 1) | (1 << 25), 0));
        assert_eq!(report.fault_type, "Multiple");
    }

    #[test]
    fn exc_return_is_interpreted() {
        let mut s = snap(0, 0);
        s.lr_exc_return = Some(0xFFFF_FFF1); // handler, basic frame
        let report = decode_fault(&s);
        let text = report.exc_return.unwrap();
        assert!(text.contains("handler"));
        assert!(text.contains("basic frame"));
    }

    #[test]
    fn snapshot_json_roundtrips() {
        let mut s = snap(0x1_0002, 1 << 30);
        s.mmfar = Some(0x2000_0000);
        s.lr_exc_return = Some(0xFFFF_FFF9);
        let json = serde_json::to_string(&s).unwrap();
        let back: FaultSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
        assert_eq!(decode_fault(&s), decode_fault(&back));
    }

    #[test]
    fn stack_frame_labels_match_layout() {
        assert_eq!(
            stack_frame_labels(),
            ["R0", "R1", "R2", "R3", "R12", "LR", "PC", "xPSR"]
        );
    }
}
