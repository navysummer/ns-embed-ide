//! PC-sample profiler aggregation.
//!
//! The trace stream (see [`crate::swvd`]) can deliver DWT PC samples — the
//! program counter sampled periodically by a hardware comparator. This module
//! accumulates those samples over a session and attributes each PC to the
//! enclosing function symbol (from `nm`), producing a hotspot histogram.
//!
//! The aggregator is pure and testable: call [`ProfileAggregator::record`] with
//! each sampled PC, then [`ProfileAggregator::hotspots`] for the sorted result.

/// A single (PC, symbol) attribution, ready to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hotspot {
    /// Function (or symbol) name, or `"<unknown>"` when the PC falls outside
    /// every known symbol.
    pub name: String,
    /// Number of samples attributed to this symbol.
    pub hits: u64,
    /// The symbol's load address (0 when unknown).
    pub address: u64,
}

/// Accumulates PC samples and attributes them to function symbols.
#[derive(Debug, Clone, Default)]
pub struct ProfileAggregator {
    /// Function symbols sorted by address ascending, with their size. Used to
    /// find the enclosing function for a sampled PC.
    symbols: Vec<(u64, u64, String)>,
    /// hit count per symbol index (parallel to `symbols`).
    hits: Vec<u64>,
    /// PC samples that fell outside every known symbol.
    unknown: u64,
    /// Total samples recorded.
    total: u64,
}

impl ProfileAggregator {
    /// Build an aggregator from a sorted symbol table. `symbols` is
    /// `(address, size, name)`; the caller usually gets it from
    /// `ns_flash::firmware::parse_nm_symbols`.
    pub fn new(symbols: &[(u64, u64, String)]) -> Self {
        let mut sorted: Vec<(u64, u64, String)> = symbols.to_vec();
        sorted.sort_by_key(|(addr, _, _)| *addr);
        let hits = vec![0; sorted.len()];
        Self {
            symbols: sorted,
            hits,
            unknown: 0,
            total: 0,
        }
    }

    /// Record one sampled PC.
    pub fn record(&mut self, pc: u32) {
        self.total += 1;
        let pc = pc as u64;
        // Binary search for the last symbol whose address is <= pc.
        let idx = self.symbols.partition_point(|(addr, _, _)| *addr <= pc);
        if idx == 0 {
            self.unknown += 1;
            return;
        }
        let candidate = idx - 1;
        let (addr, size, _) = &self.symbols[candidate];
        // The sample belongs to the symbol if it falls inside [addr, addr+size);
        // otherwise it's a gap (e.g. between functions) — attribute to unknown.
        if pc < addr + size {
            self.hits[candidate] += 1;
        } else {
            self.unknown += 1;
        }
    }

    /// Total samples recorded.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Samples that fell outside every known symbol.
    pub fn unknown(&self) -> u64 {
        self.unknown
    }

    /// Hotspots sorted by hit count descending (only symbols with hits > 0).
    pub fn hotspots(&self) -> Vec<Hotspot> {
        let mut result: Vec<Hotspot> = Vec::new();
        for (index, (addr, _, name)) in self.symbols.iter().enumerate() {
            let hits = self.hits[index];
            if hits > 0 {
                result.push(Hotspot {
                    name: name.clone(),
                    hits,
                    address: *addr,
                });
            }
        }
        if self.unknown > 0 {
            result.push(Hotspot {
                name: "<unknown>".to_string(),
                hits: self.unknown,
                address: 0,
            });
        }
        result.sort_by(|a, b| b.hits.cmp(&a.hits));
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_symbols() -> Vec<(u64, u64, String)> {
        vec![
            (0x0800_0000, 0x100, "Reset_Handler".to_string()),
            (0x0800_0100, 0x80, "main".to_string()),
            (0x0800_0180, 0x40, "blink".to_string()),
        ]
    }

    #[test]
    fn attributes_samples_to_enclosing_function() {
        let mut agg = ProfileAggregator::new(&sample_symbols());
        agg.record(0x0800_0020); // Reset_Handler
        agg.record(0x0800_0110); // main
        agg.record(0x0800_0110); // main
        agg.record(0x0800_0190); // blink
        agg.record(0x0800_0190); // blink
        agg.record(0x0800_0190); // blink
        let hotspots = agg.hotspots();
        assert_eq!(hotspots.len(), 3);
        assert_eq!(hotspots[0].name, "blink");
        assert_eq!(hotspots[0].hits, 3);
        assert_eq!(hotspots[1].name, "main");
        assert_eq!(hotspots[1].hits, 2);
        assert_eq!(hotspots[2].name, "Reset_Handler");
        assert_eq!(hotspots[2].hits, 1);
        assert_eq!(agg.total(), 6);
        assert_eq!(agg.unknown(), 0);
    }

    #[test]
    fn gap_and_unknown_samples() {
        // A PC before the first symbol is unknown.
        let mut agg = ProfileAggregator::new(&sample_symbols());
        agg.record(0x0000_0000);
        assert_eq!(agg.unknown(), 1);
        assert_eq!(agg.hotspots()[0].name, "<unknown>");
        assert_eq!(agg.hotspots()[0].hits, 1);
    }

    #[test]
    fn empty_symbols_puts_everything_in_unknown() {
        let mut agg = ProfileAggregator::new(&[]);
        agg.record(0x0800_0100);
        agg.record(0x0800_0200);
        assert_eq!(agg.unknown(), 2);
        assert_eq!(agg.total(), 2);
        assert_eq!(agg.hotspots()[0].name, "<unknown>");
        assert_eq!(agg.hotspots()[0].hits, 2);
    }
}
