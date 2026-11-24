//! Waveform data model for the SWV/ITM trace stream.
//!
//! This module turns the decoded [`Packet`](crate::swvd::Packet) stream into
//! plottable channels without depending on GPUI or any UI state. Keeping the
//! transformation pure means it can be unit-tested exhaustively and reused by
//! both the IDE waveform tab and the MCP `trace_waveform` tool.
//!
//! # Time axis
//!
//! The trace buffer in the IDE is an append-only `Vec<Packet>` capped at a
//! fixed length. Packets carry an optional ITM `Timestamp` value, but the
//! target is normally configured *without* the timestamp counter, so there is
//! no guaranteed real-time axis. Consequently a channel sample's `t` is the
//! **packet index** (a relative, non-uniform axis), which the UI must label as
//! such. When real timestamps happen to be present they are preserved in
//! [`Channel::has_timestamp`] and can be used for local ticks.

use crate::swvd::Packet;

/// A selectable waveform channel derived from the trace packet stream.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChannelKind {
    /// One ITM stimulus port; each payload byte becomes one sample.
    Stimulus { port: u8 },
    /// DWT PC-sampling events; the sample value is the low 16 bits of the PC.
    DwtPc,
    /// Entry/exit of one exception number; value is 1 (entered) or 0 (exited).
    Exception { number: u16 },
    /// ITM timestamp packets; value is the raw 32-bit counter (scaled for plot).
    Timestamp,
}

impl ChannelKind {
    /// Stable identifier used as a UI element id and in MCP output.
    pub fn id(&self) -> String {
        match self {
            ChannelKind::Stimulus { port } => format!("stim{}", port),
            ChannelKind::DwtPc => "dwt_pc".to_string(),
            ChannelKind::Exception { number } => format!("exc{}", number),
            ChannelKind::Timestamp => "ts".to_string(),
        }
    }

    /// Human-readable label shown in the waveform legend.
    pub fn label(&self) -> String {
        match self {
            ChannelKind::Stimulus { port } => format!("ITM{}", port),
            ChannelKind::DwtPc => "PC".to_string(),
            ChannelKind::Exception { number } => format!("EXC{}", number),
            ChannelKind::Timestamp => "TS".to_string(),
        }
    }

    /// Parse a stable channel id (as produced by [`ChannelKind::id`]) back into
    /// its kind. Returns `None` for unknown ids so callers can ignore stale
    /// selections instead of erroring the whole channel build.
    pub fn from_id(id: &str) -> Option<Self> {
        let id = id.trim();
        if id == "dwt_pc" {
            return Some(ChannelKind::DwtPc);
        }
        if id == "ts" {
            return Some(ChannelKind::Timestamp);
        }
        if let Some(port) = id.strip_prefix("stim") {
            if let Ok(port) = port.parse::<u8>() {
                return Some(ChannelKind::Stimulus { port });
            }
        }
        if let Some(number) = id.strip_prefix("exc") {
            if let Ok(number) = number.parse::<u16>() {
                return Some(ChannelKind::Exception { number });
            }
        }
        None
    }
}

/// One plotted point. `t` is the packet index (relative time), `value` is the
/// normalized amplitude in `0.0..=1.0`, and `raw` is the original value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub t: u64,
    pub value: f32,
    pub raw: u64,
}

/// A fully built channel ready for rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    pub kind: ChannelKind,
    pub label: String,
    pub samples: Vec<Sample>,
    /// True when at least one backing packet carried a real ITM timestamp.
    pub has_timestamp: bool,
}

/// Result of transforming a packet slice into channels.
#[derive(Debug, Clone, PartialEq)]
pub struct WaveformData {
    pub channels: Vec<Channel>,
    /// Total number of packets examined (the x-axis upper bound).
    pub packet_count: usize,
    /// True when any timestamp packet was seen in the source stream.
    pub has_timestamps: bool,
}

/// Discover the channels actually present in a packet slice.
///
/// Ports, exception numbers and DWT/Timestamp presence are gathered in a
/// single pass. The result is sorted deterministically (ports/numbers
/// ascending, then DWT, then timestamp) so the UI legend is stable across
/// refreshes.
pub fn available_channels(packets: &[Packet]) -> Vec<ChannelKind> {
    let mut ports = std::collections::BTreeSet::new();
    let mut exceptions = std::collections::BTreeSet::new();
    let mut has_pc = false;
    let mut has_ts = false;
    for packet in packets {
        match packet {
            Packet::Stimulus { port, .. } => {
                ports.insert(*port);
            }
            Packet::Dwt { pc: Some(_), .. } => has_pc = true,
            Packet::Exception { number, .. } => {
                exceptions.insert(*number);
            }
            Packet::Timestamp(_) => has_ts = true,
            Packet::Dwt { pc: None, .. } => {}
        }
    }
    let mut kinds: Vec<ChannelKind> = ports
        .into_iter()
        .map(|port| ChannelKind::Stimulus { port })
        .collect();
    if has_pc {
        kinds.push(ChannelKind::DwtPc);
    }
    kinds.extend(
        exceptions
            .into_iter()
            .map(|number| ChannelKind::Exception { number }),
    );
    if has_ts {
        kinds.push(ChannelKind::Timestamp);
    }
    kinds
}

/// Build chart-ready channels from trace packets and a requested channel list.
///
/// Unknown kinds in `kinds` are ignored; channels with no samples are still
/// returned so the UI legend stays stable while the stream is starting up.
/// A packet may contribute to several channels (e.g. an ITM timestamp plus a
/// stimulus byte stream), which is why each channel keeps its own `samples`.
pub fn build_channels(packets: &[Packet], kinds: &[ChannelKind]) -> WaveformData {
    let mut channels: Vec<Channel> = kinds
        .iter()
        .map(|kind| Channel {
            kind: kind.clone(),
            label: kind.label(),
            samples: Vec::new(),
            has_timestamp: false,
        })
        .collect();

    let mut has_timestamps = false;
    let mut last_ts: Option<u32> = None;

    for (index, packet) in packets.iter().enumerate() {
        let t = index as u64;
        if let Packet::Timestamp(ts) = packet {
            has_timestamps = true;
            last_ts = Some(*ts);
        }
        for channel in &mut channels {
            let sample = match (&channel.kind, packet) {
                (ChannelKind::Stimulus { port }, Packet::Stimulus { port: p, payload })
                    if port == p =>
                {
                    // Only the first byte of a stimulus packet is plotted as a
                    // single channel value; the full payload remains visible
                    // in the raw trace list. This keeps one point per packet
                    // and a uniform x axis across channels.
                    payload.first().map(|b| Sample {
                        t,
                        value: *b as f32 / u8::MAX as f32,
                        raw: *b as u64,
                    })
                }
                (ChannelKind::DwtPc, Packet::Dwt { pc: Some(pc), .. }) => Some(Sample {
                    t,
                    // Show the low 12 bits so nearby code addresses produce a
                    // readable stepped trace instead of a flat line.
                    value: (*pc & 0x0FFF) as f32 / 0x0FFF as f32,
                    raw: *pc as u64,
                }),
                (ChannelKind::Exception { number }, Packet::Exception { number: n, entered })
                    if number == n =>
                {
                    Some(Sample {
                        t,
                        value: if *entered { 1.0 } else { 0.0 },
                        raw: if *entered { 1 } else { 0 },
                    })
                }
                (ChannelKind::Timestamp, Packet::Timestamp(ts)) => Some(Sample {
                    t,
                    value: (*ts & 0xFFFF) as f32 / u16::MAX as f32,
                    raw: *ts as u64,
                }),
                _ => None,
            };
            if let Some(s) = sample {
                if last_ts.is_some() {
                    channel.has_timestamp = true;
                }
                channel.samples.push(s);
            }
        }
    }

    WaveformData {
        channels,
        packet_count: packets.len(),
        has_timestamps,
    }
}

/// Reduce a channel to at most `max_points` samples by uniform striding.
///
/// The first and last samples are always preserved so the plotted span does
/// not shrink. Rendering uses this to bound the number of path segments when
/// the trace buffer holds thousands of packets.
pub fn decimate(samples: &[Sample], max_points: usize) -> Vec<Sample> {
    if max_points == 0 {
        return Vec::new();
    }
    if samples.len() <= max_points {
        return samples.to_vec();
    }
    if max_points == 1 {
        return samples.first().copied().into_iter().collect();
    }
    let stride = (samples.len() + max_points - 2) / (max_points - 1);
    let mut out: Vec<Sample> = samples.iter().step_by(stride).copied().collect();
    let last = samples[samples.len() - 1];
    if out.last().copied() != Some(last) {
        out.push(last);
    }
    out.truncate(max_points);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stim(port: u8, bytes: &[u8]) -> Packet {
        Packet::Stimulus {
            port,
            payload: bytes.to_vec(),
        }
    }

    #[test]
    fn available_channels_discovers_all_kinds_in_order() {
        let packets = vec![
            stim(3, &[1, 2]),
            Packet::Exception {
                number: 4,
                entered: true,
            },
            Packet::Dwt {
                index: 0,
                pc: Some(0x0800_1234),
            },
            stim(1, &[9]),
            Packet::Timestamp(500),
            Packet::Dwt { index: 0, pc: None },
        ];
        let kinds = available_channels(&packets);
        assert_eq!(
            kinds,
            vec![
                ChannelKind::Stimulus { port: 1 },
                ChannelKind::Stimulus { port: 3 },
                ChannelKind::DwtPc,
                ChannelKind::Exception { number: 4 },
                ChannelKind::Timestamp,
            ]
        );
    }

    #[test]
    fn stimulus_channel_maps_first_byte_to_normalized_sample() {
        let packets = vec![stim(0, &[0]), stim(0, &[128]), stim(0, &[255])];
        let data = build_channels(&packets, &[ChannelKind::Stimulus { port: 0 }]);
        assert_eq!(data.packet_count, 3);
        let ch = &data.channels[0];
        assert_eq!(ch.label, "ITM0");
        assert_eq!(ch.samples.len(), 3);
        assert_eq!(ch.samples[0].raw, 0);
        assert_eq!(ch.samples[1].raw, 128);
        assert_eq!(ch.samples[2].raw, 255);
        assert!((ch.samples[1].value - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(ch.samples[2].value, 1.0);
        assert_eq!(ch.samples[2].t, 2);
    }

    #[test]
    fn stimulus_channels_are_isolated_by_port() {
        let packets = vec![stim(1, &[10]), stim(2, &[20]), stim(1, &[30])];
        let data = build_channels(
            &packets,
            &[
                ChannelKind::Stimulus { port: 1 },
                ChannelKind::Stimulus { port: 2 },
            ],
        );
        assert_eq!(data.channels[0].samples.len(), 2);
        assert_eq!(data.channels[1].samples.len(), 1);
        assert_eq!(data.channels[0].samples[1].raw, 30);
        assert_eq!(data.channels[1].samples[0].raw, 20);
    }

    #[test]
    fn dwt_pc_channel_keeps_raw_pc_and_masks_low_bits() {
        let packets = vec![
            Packet::Dwt {
                index: 0,
                pc: Some(0x0800_0ABC),
            },
            Packet::Dwt {
                index: 0,
                pc: Some(0x0800_0000),
            },
            Packet::Dwt { index: 0, pc: None },
        ];
        let data = build_channels(&packets, &[ChannelKind::DwtPc]);
        let ch = &data.channels[0];
        assert_eq!(ch.samples.len(), 2);
        assert_eq!(ch.samples[0].raw, 0x0800_0ABC);
        assert_eq!(ch.samples[0].t, 0);
        assert_eq!(ch.samples[1].raw, 0x0800_0000);
        assert_eq!(ch.samples[1].t, 1);
        assert_eq!(ch.samples[1].value, 0.0);
    }

    #[test]
    fn exception_channel_is_digital() {
        let packets = vec![
            Packet::Exception {
                number: 3,
                entered: true,
            },
            Packet::Exception {
                number: 4,
                entered: true,
            },
            Packet::Exception {
                number: 3,
                entered: false,
            },
        ];
        let data = build_channels(&packets, &[ChannelKind::Exception { number: 3 }]);
        let samples = &data.channels[0].samples;
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].value, 1.0);
        assert_eq!(samples[1].value, 0.0);
        assert_eq!(samples[1].t, 2);
    }

    #[test]
    fn timestamp_packets_are_tracked_and_plotted() {
        let packets = vec![stim(0, &[7]), Packet::Timestamp(0x0001_2345), stim(0, &[8])];
        let data = build_channels(
            &packets,
            &[ChannelKind::Stimulus { port: 0 }, ChannelKind::Timestamp],
        );
        assert!(data.has_timestamps);
        // All channels are flagged once any timestamp was seen, so the UI can
        // choose to draw real-time ticks.
        assert!(data.channels[0].has_timestamp);
        assert_eq!(data.channels[1].samples.len(), 1);
        assert_eq!(data.channels[1].samples[0].raw, 0x1_2345);
        assert_eq!(data.channels[1].samples[0].t, 1);
    }

    #[test]
    fn kinds_without_samples_are_still_returned() {
        let data = build_channels(
            &[],
            &[ChannelKind::Stimulus { port: 9 }, ChannelKind::DwtPc],
        );
        assert_eq!(data.channels.len(), 2);
        assert!(data.channels.iter().all(|c| c.samples.is_empty()));
        assert!(!data.has_timestamps);
    }

    #[test]
    fn channel_id_and_label_are_stable() {
        assert_eq!(ChannelKind::Stimulus { port: 12 }.id(), "stim12");
        assert_eq!(ChannelKind::DwtPc.id(), "dwt_pc");
        assert_eq!(ChannelKind::Exception { number: 3 }.id(), "exc3");
        assert_eq!(ChannelKind::Timestamp.id(), "ts");
        assert_eq!(ChannelKind::Exception { number: 3 }.label(), "EXC3");
    }

    #[test]
    fn decimate_preserves_endpoints_and_bounds_count() {
        let samples: Vec<Sample> = (0..1000)
            .map(|i| Sample {
                t: i,
                value: 0.5,
                raw: i,
            })
            .collect();
        let small = decimate(&samples, 10);
        assert!(small.len() <= 10);
        assert_eq!(small.first().unwrap().t, 0);
        assert_eq!(small.last().unwrap().t, 999);

        assert_eq!(decimate(&samples, 0).len(), 0);
        assert_eq!(decimate(&samples, 2000).len(), 1000);
        assert_eq!(decimate(&samples[..2], 1).len(), 1);
    }
}
