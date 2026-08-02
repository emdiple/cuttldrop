//! The outer erasure code (`DESIGN.md` §1a, §3c).
//!
//! No back channel exists, so there are no ACKs and no retransmit requests. The
//! skin loops forever and the eye reconstructs from any sufficient subset —
//! which is exactly what a rateless fountain code provides.
//!
//! ## Why RaptorQ and not LT
//!
//! Not because of overhead. The optical channel loses 40–70% of symbols, which
//! swamps the difference between LT's ~5–30% reception overhead and RaptorQ's
//! ~2%. RaptorQ wins on *implementation risk*: RFC 6330 is a spec with a mature
//! crate behind it, whereas hand-rolled LT means owning Robust Soliton tuning,
//! and a mistuned LT is silently twice as bad with no error message.
//!
//! ## The seam
//!
//! [`Fountain`] and [`Sink`] are the swap point. Nothing outside this module
//! mentions RaptorQ, so an LT A/B is a second impl rather than a refactor.
//!
//! One thing that emphatically does *not* belong here: error correction. A
//! fountain code repairs **erasures**, not errors, and a single corrupt symbol
//! propagates through the XOR graph and poisons the whole object. Keeping
//! errors out is the CRC gate's job, upstream in [`crate::stream`] (§1b).

use crate::error::{Error, Result};

/// Bytes of codec configuration the eye needs before it can decode anything.
/// For RaptorQ this is the RFC 6330 Object Transmission Information.
pub const CONFIG_LEN: usize = 12;

/// Bytes each symbol carries as its own identifier, on top of the payload.
pub const SYMBOL_ID_LEN: usize = 4;

/// RFC 6330 symbol alignment: symbol sizes must be a multiple of this, which is
/// why the chosen symbol is usually smaller than the space a pulse offers.
const ALIGNMENT: u8 = 8;

/// RFC 6330 §4.4.1.2 / errata 5548 bounds, used to reject implausible configs.
const MAX_TRANSFER_LENGTH: u64 = 942_574_504_275;
const MAX_SOURCE_SYMBOLS_PER_BLOCK: u64 = 56_403;

/// Skin side: turns an object into an effectively unbounded symbol stream.
pub trait Fountain: Sized {
    /// `max_symbol` is what one pulse can carry. Implementations may choose a
    /// smaller symbol (RaptorQ aligns down), so never assume the two are equal —
    /// read the real size back from the [`Sink`].
    fn new(object: &[u8], max_symbol: u16) -> Result<Self>;

    fn config(&self) -> [u8; CONFIG_LEN];

    /// Source symbols plus `overhead` × K repair symbols.
    ///
    /// A finite batch is an M0 convenience: the CLI writes a directory of PNGs,
    /// which has to stand in for a skin that loops indefinitely. The browser
    /// skin will pull repair symbols on demand instead.
    fn symbols(&self, overhead: f32) -> Vec<Vec<u8>>;
}

/// Eye side: absorbs symbols until the object falls out.
pub trait Sink: Sized {
    /// Validate a config and report its symbol length, allocating nothing.
    ///
    /// This exists because of an ordering problem with real teeth: the framing
    /// layer needs the symbol length to know how many bytes the CRC covers, so
    /// it must read the config *before* the CRC has vouched for it. On a noisy
    /// channel that config is sometimes garbage. Handing garbage to a decoder
    /// gets you a panic deep inside it — so nothing untrusted may reach one.
    /// `probe` is the gate: implausible configs are rejected here, and the
    /// pulse becomes an ordinary erasure.
    fn probe(config: &[u8; CONFIG_LEN]) -> Option<usize>;

    fn new(config: &[u8; CONFIG_LEN]) -> Result<Self>;

    /// Payload bytes per symbol, excluding [`SYMBOL_ID_LEN`].
    fn symbol_len(&self) -> usize;

    /// Minimum symbols needed — the denominator of honest progress (§3c).
    fn source_symbols(&self) -> u32;

    /// Returns the object once enough symbols have arrived.
    fn absorb(&mut self, symbol: &[u8]) -> Option<Vec<u8>>;
}

/// RFC 6330 RaptorQ.
pub struct RaptorQ {
    config: raptorq::ObjectTransmissionInformation,
    encoder: Option<raptorq::Encoder>,
    empty: bool,
}

impl Fountain for RaptorQ {
    fn new(object: &[u8], max_symbol: u16) -> Result<Self> {
        if max_symbol < ALIGNMENT as u16 {
            return Err(Error::BadConfig);
        }
        // A zero-length object has no symbols to encode, and raptorq cannot
        // even describe one: `with_defaults(0, _)` divides by zero deriving the
        // symbol count. Build the config directly instead — `new` guards that
        // division — and let the transfer length of 0 tell the eye the whole
        // story. An empty file is a legitimate thing to send.
        if object.is_empty() {
            let symbol_size = max_symbol / ALIGNMENT as u16 * ALIGNMENT as u16;
            return Ok(Self {
                config: raptorq::ObjectTransmissionInformation::new(
                    0,
                    symbol_size,
                    1,
                    1,
                    ALIGNMENT,
                ),
                encoder: None,
                empty: true,
            });
        }
        let encoder = raptorq::Encoder::with_defaults(object, max_symbol);
        Ok(Self {
            config: encoder.get_config(),
            encoder: Some(encoder),
            empty: false,
        })
    }

    fn config(&self) -> [u8; CONFIG_LEN] {
        self.config.serialize()
    }

    fn symbols(&self, overhead: f32) -> Vec<Vec<u8>> {
        let Some(encoder) = &self.encoder else {
            // Still emit one pulse so the eye learns the config and the stream
            // id; the object itself is empty.
            return vec![Vec::new()];
        };
        let source = source_symbol_count(&self.config);
        let repair = (source as f32 * overhead.max(0.0)).ceil() as u32;
        encoder
            .get_encoded_packets(repair)
            .into_iter()
            .map(|packet| packet.serialize())
            .collect()
    }
}

impl RaptorQ {
    pub fn is_empty(&self) -> bool {
        self.empty
    }
}

pub struct RaptorQSink {
    config: raptorq::ObjectTransmissionInformation,
    /// `None` for an empty object — raptorq's decoder cannot be constructed for
    /// a zero transfer length.
    decoder: Option<raptorq::Decoder>,
    source: u32,
}

impl Sink for RaptorQSink {
    fn probe(config: &[u8; CONFIG_LEN]) -> Option<usize> {
        let config = raptorq::ObjectTransmissionInformation::deserialize(config);
        let transfer = config.transfer_length();
        if transfer == 0 {
            return Some(0);
        }
        // Mirrors the assertions inside raptorq's own constructor. Anything
        // that would trip one of those must not get that far.
        let (symbol, alignment) = (config.symbol_size(), config.symbol_alignment());
        let (blocks, sub) = (config.source_blocks(), config.sub_blocks());
        if transfer > MAX_TRANSFER_LENGTH
            || symbol == 0
            || alignment == 0
            || blocks == 0
            || sub == 0
            || !symbol.is_multiple_of(alignment as u16)
        {
            return None;
        }
        let per_block = transfer.div_ceil(symbol as u64).div_ceil(blocks as u64);
        (per_block <= MAX_SOURCE_SYMBOLS_PER_BLOCK).then_some(symbol as usize)
    }

    fn new(config: &[u8; CONFIG_LEN]) -> Result<Self> {
        Self::probe(config).ok_or(Error::BadConfig)?;
        let config = raptorq::ObjectTransmissionInformation::deserialize(config);
        let empty = config.transfer_length() == 0;
        Ok(Self {
            decoder: (!empty).then(|| raptorq::Decoder::new(config)),
            source: source_symbol_count(&config),
            config,
        })
    }

    /// Zero for an empty object: there is no symbol to carry, and the framing
    /// layer relies on that to size — and CRC — the pulse body correctly.
    fn symbol_len(&self) -> usize {
        match self.decoder {
            Some(_) => self.config.symbol_size() as usize,
            None => 0,
        }
    }

    fn source_symbols(&self) -> u32 {
        self.source
    }

    fn absorb(&mut self, symbol: &[u8]) -> Option<Vec<u8>> {
        match &mut self.decoder {
            None => Some(Vec::new()),
            Some(decoder) => decoder.decode(raptorq::EncodingPacket::deserialize(symbol)),
        }
    }
}

fn source_symbol_count(config: &raptorq::ObjectTransmissionInformation) -> u32 {
    let symbol = config.symbol_size() as u64;
    if symbol == 0 {
        return 0;
    }
    config.transfer_length().div_ceil(symbol) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_of(object: &[u8], max_symbol: u16) -> [u8; CONFIG_LEN] {
        RaptorQ::new(object, max_symbol).unwrap().config()
    }

    #[test]
    fn symbol_size_is_not_the_mtu() {
        // RaptorQ aligns the symbol size down, so the eye must read it from the
        // config rather than assuming it matches what the skin asked for. This
        // test exists because assuming otherwise silently truncates symbols.
        let sink = RaptorQSink::new(&config_of(&[0u8; 4096], 83)).unwrap();
        assert!(sink.symbol_len() <= 83);
        assert!(sink.symbol_len() > 0);
    }

    #[test]
    fn decodes_from_a_sufficient_subset() {
        let object: Vec<u8> = (0..4096u32).map(|i| (i * 7) as u8).collect();
        let fountain = RaptorQ::new(&object, 80).unwrap();
        let symbols = fountain.symbols(1.0);
        let mut sink = RaptorQSink::new(&fountain.config()).unwrap();

        // Drop every other symbol: half the stream, arbitrarily chosen.
        let mut out = None;
        for symbol in symbols.iter().step_by(2) {
            if let Some(object) = sink.absorb(symbol) {
                out = Some(object);
                break;
            }
        }
        assert_eq!(out.unwrap(), object);
    }

    /// A corrupt config must be rejected, not handed to a decoder. Before
    /// `probe` existed, noisy channels drove raptorq into a divide-by-zero.
    #[test]
    fn implausible_configs_are_rejected_without_panicking() {
        let poison: [[u8; CONFIG_LEN]; 4] = [
            // Non-zero transfer length, zero symbol size.
            [0, 0, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            // Zero source blocks.
            [0, 0, 16, 0, 64, 0, 0, 0, 0, 0, 1, 8],
            // Symbol size not a multiple of the alignment.
            [0, 0, 16, 0, 65, 1, 0, 1, 0, 0, 1, 8],
            // Absurd transfer length.
            [0xFF, 0xFF, 0xFF, 0xFF, 64, 1, 0, 1, 0, 0, 1, 8],
        ];
        for config in &poison {
            assert_eq!(RaptorQSink::probe(config), None, "config {config:?}");
            assert!(RaptorQSink::new(config).is_err());
        }
    }

    /// Every config we ourselves emit must survive its own validator.
    #[test]
    fn our_own_configs_always_probe_clean() {
        for len in [1usize, 7, 80, 1024, 65_536] {
            let config = config_of(&vec![0xA5; len], 80);
            assert!(RaptorQSink::probe(&config).is_some(), "len {len}");
        }
        assert_eq!(RaptorQSink::probe(&config_of(&[], 80)), Some(0));
    }

    #[test]
    fn empty_object_survives() {
        let fountain = RaptorQ::new(&[], 80).unwrap();
        assert!(fountain.is_empty());
        let mut sink = RaptorQSink::new(&fountain.config()).unwrap();
        assert_eq!(sink.absorb(&[]), Some(Vec::new()));
    }

    #[test]
    fn overhead_scales_the_symbol_count() {
        let object = vec![1u8; 8192];
        let fountain = RaptorQ::new(&object, 80).unwrap();
        let lean = fountain.symbols(0.0).len();
        let fat = fountain.symbols(2.0).len();
        assert!(fat > lean * 2, "overhead 2.0 gave {fat} vs {lean} symbols");
    }
}
