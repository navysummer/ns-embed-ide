//! DWT PC-sample hotspot aggregation for the trace panel's profiler view.
//!
//! The trace adapter records the sampled PC (program counter) for every DWT
//! PC-sampling packet. This module attributes those samples to firmware
//! functions using the `nm` symbol table: each sample address is located in
//! the function whose `[address, address + size)` range contains it (binary
//! search). Samples that fall in a gap between symbols, or before the first
//! symbol, are attributed to the reserved `<unknown>` bucket so they remain
//! visible rather than being silently dropped.
//!
//! The aggregator is rebuilt (with a fresh symbol table) whenever the trace
//! panel's「热点」toggle is turned on, so already-recorded samples are
//! re-attributed where possible.

/// A single hotspot: a function (or `<unknown>`) with its sample count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hotspot {
    pub name: String,
    pub hits: u64,
}

/// A symbol from the firmware's `nm` table: `[address, address + size)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SymbolRange {
    address: u64,
    size: u64,
}

/// Aggregate sampled PCs into per-function hit counts.
///
/// The symbol table is sorted by address and kept in a flat `Vec` so lookup is
/// a binary search over address. Samples are recorded by `record`; `hotspots`
/// returns the counts sorted by hits descending (ties broken by name).
pub struct ProfileAggregator {
    symbols: Vec<SymbolRange>,
    /// Total samples recorded.
    total: u64,
    /// Hit count per symbol index (`<unknown>` is index `symbols.len()`).
    hits: Vec<u64>,
    /// Function names, parallel to `symbols`, plus the final `<unknown>` label.
    names: Vec<String>,
}

impl ProfileAggregator {
    /// Build an aggregator from `(address, size, name)` symbol tuples. The
    /// tuples may be in any order; they are sorted internally.
    pub fn new(symbols: &[(u64, u64, String)]) -> Self {
        let mut sorted: Vec<(u64, u64, String)> = symbols.to_vec();
        sorted.sort_by_key(|(address, _, _)| *address);
        let mut ranges = Vec::with_capacity(sorted.len());
        let mut names = Vec::with_capacity(sorted.len());
        for (address, size, name) in sorted {
            if size == 0 {
                continue;
            }
            ranges.push(SymbolRange { address, size });
            names.push(name);
        }
        let hit_count = ranges.len() + 1; // +1 for `<unknown>`.
        Self {
            symbols: ranges,
            total: 0,
            hits: vec![0; hit_count],
            names,
        }
    }

    /// Record one sampled program counter.
    pub fn record(&mut self, pc: u32) {
        self.total = self.total.saturating_add(1);
        let index = self.locate(pc as u64);
        if let Some(slot) = self.hits.get_mut(index) {
            *slot = slot.saturating_add(1);
        }
    }

    /// Total number of samples recorded.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// The hotspots sorted by hits descending (ties broken by name). Includes
    /// the `<unknown>` bucket only when it has at least one hit.
    pub fn hotspots(&self) -> Vec<Hotspot> {
        let mut out: Vec<Hotspot> = self
            .hits
            .iter()
            .enumerate()
            .filter(|(_, hits)| **hits > 0)
            .map(|(index, hits)| Hotspot {
                name: self.name_for(index),
                hits: *hits,
            })
            .collect();
        out.sort_by(|a, b| b.hits.cmp(&a.hits).then_with(|| a.name.cmp(&b.name)));
        out
    }

    /// Name of the bucket at `index` (`<unknown>` for the trailing slot).
    fn name_for(&self, index: usize) -> String {
        self.names.get(index).cloned().unwrap_or_else(|| "<unknown>".to_string())
    }

    /// Binary-search `pc` into the sorted symbol ranges. Returns the symbol
    /// index, or `self.symbols.len()` for `<unknown>` (gap / before first).
    fn locate(&self, pc: u64) -> usize {
        let mut lo = 0usize;
        let mut hi = self.symbols.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            let range = &self.symbols[mid];
            if pc < range.address {
                hi = mid;
            } else if pc >= range.address + range.size {
                lo = mid + 1;
            } else {
                return mid;
            }
        }
        self.symbols.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbols() -> Vec<(u64, u64, String)> {
        vec![
            (0x0800_0000, 0x100, "main".to_string()),
            (0x0800_0100, 0x80, "gpio_init".to_string()),
            (0x0800_0200, 0x40, "delay".to_string()),
        ]
    }

    /// Samples inside a symbol's range are attributed to that function; ties
    /// in hits are broken by name and the list is sorted by hits descending.
    #[test]
    fn aggregates_pc_samples_to_functions() {
        let mut agg = ProfileAggregator::new(&symbols());
        agg.record(0x0800_0010); // main
        agg.record(0x0800_0020); // main
        agg.record(0x0800_0110); // gpio_init
        agg.record(0x0800_0110); // gpio_init
        agg.record(0x0800_0110); // gpio_init
        agg.record(0x0800_0210); // delay

        assert_eq!(agg.total(), 6);
        let hotspots = agg.hotspots();
        assert_eq!(hotspots.len(), 3);
        // Sorted by hits descending: gpio_init (3), main (2), delay (1).
        assert_eq!(hotspots[0].name, "gpio_init");
        assert_eq!(hotspots[0].hits, 3);
        assert_eq!(hotspots[1].name, "main");
        assert_eq!(hotspots[1].hits, 2);
        assert_eq!(hotspots[2].name, "delay");
        assert_eq!(hotspots[2].hits, 1);
    }

    /// Samples in gaps or before the first symbol fall into `<unknown>` and
    /// are still reported (not silently dropped).
    #[test]
    fn gap_and_pre_first_samples_go_to_unknown() {
        let mut agg = ProfileAggregator::new(&symbols());
        agg.record(0x0800_0190); // gap between gpio_init end and delay start
        agg.record(0x07FF_FFF0); // before first symbol
        agg.record(0x0800_0010); // main

        assert_eq!(agg.total(), 3);
        let hotspots = agg.hotspots();
        // `<unknown>` has 2 hits (top), main 1.
        assert_eq!(hotspots[0].name, "<unknown>");
        assert_eq!(hotspots[0].hits, 2);
        assert_eq!(hotspots[1].name, "main");
        assert_eq!(hotspots[1].hits, 1);
    }

    /// With an empty symbol table every sample goes to `<unknown>`; a fresh
    /// aggregator reports zero total and no hotspots.
    #[test]
    fn empty_symbols_and_no_samples() {
        let mut agg = ProfileAggregator::new(&[]);
        assert_eq!(agg.total(), 0);
        assert!(agg.hotspots().is_empty());

        agg.record(0x1234);
        assert_eq!(agg.total(), 1);
        let hotspots = agg.hotspots();
        assert_eq!(hotspots.len(), 1);
        assert_eq!(hotspots[0].name, "<unknown>");
        assert_eq!(hotspots[0].hits, 1);
    }
}
