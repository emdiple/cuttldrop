//! What the file *is*: name, mime type, and the BLAKE3 hash that verifies it
//! (`DESIGN.md` §3c).
//!
//! The manifest is the eye's only true correctness check. Every other layer —
//! inner RS, the CRC gate, the fountain — exists to deliver bytes; this is the
//! one that says whether the delivered bytes are *the file*. `Receiver::finish`
//! refuses to hand anything back until the reconstruction matches this hash
//! (§3f: never return an unverified file).
//!
//! ## How it travels
//!
//! §3c sketched the manifest as a second fountain-coded stream. What landed is
//! simpler, deliberately: the whole manifest fits in **one** symbol, and a
//! fountain code over a single-symbol object degenerates to repetition — so the
//! skin just repeats it, donating band 0's symbol slot every
//! [`crate::stream::MANIFEST_PERIOD`]-th pulse, flagged in the stream header
//! and protected by the same RS + CRC path as any symbol. Same delivery
//! guarantee, no second OTI, no second decoder.
//!
//! The file *size* is deliberately absent: the RFC 6330 OTI in every stream
//! header already carries the exact transfer length, so repeating it here would
//! be a second copy that could disagree.

/// BLAKE3 output length.
pub const HASH_LEN: usize = 32;

/// Longest name carried, in bytes. Sized so the manifest fits band 0 of the
/// smallest profile alongside the stream header and CRC.
pub const MAX_NAME: usize = 100;

/// Longest mime type carried, in bytes. Real types run ~10–70 bytes; anything
/// longer is noise.
pub const MAX_MIME: usize = 32;

/// Name, mime and hash of the object in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// BLAKE3 of the whole object. The first four bytes also ride in every
    /// stream header, binding pulses to the manifest they belong with.
    pub hash: [u8; HASH_LEN],
    /// Suggested filename, as the sender gave it. Untrusted: anything that
    /// touches a filesystem or a download attribute goes through
    /// [`Manifest::safe_name`] instead.
    pub name: String,
    /// Mime type, or empty if the sender did not know.
    pub mime: String,
}

impl Manifest {
    /// Describe an object, hashing it and truncating the metadata to fit.
    pub fn describe(name: &str, mime: &str, object: &[u8]) -> Self {
        Self {
            hash: *blake3::hash(object).as_bytes(),
            name: truncated(name, MAX_NAME),
            mime: truncated(mime, MAX_MIME),
        }
    }

    /// Wire form: hash, then length-prefixed name and mime.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.wire_len());
        out.extend_from_slice(&self.hash);
        out.push(self.name.len() as u8);
        out.extend_from_slice(self.name.as_bytes());
        out.push(self.mime.len() as u8);
        out.extend_from_slice(self.mime.as_bytes());
        out
    }

    pub fn wire_len(&self) -> usize {
        HASH_LEN + 1 + self.name.len() + 1 + self.mime.len()
    }

    /// Parse a manifest and report how many bytes it occupied, so the caller
    /// knows where the band CRC starts. `None` for anything malformed — which
    /// the CRC gate then treats as an ordinary erasure.
    pub fn parse(bytes: &[u8]) -> Option<(Self, usize)> {
        let hash: [u8; HASH_LEN] = bytes.get(..HASH_LEN)?.try_into().ok()?;
        let name_len = *bytes.get(HASH_LEN)? as usize;
        if name_len > MAX_NAME {
            return None;
        }
        let at = HASH_LEN + 1;
        let name = core::str::from_utf8(bytes.get(at..at + name_len)?).ok()?;
        let at = at + name_len;
        let mime_len = *bytes.get(at)? as usize;
        if mime_len > MAX_MIME {
            return None;
        }
        let mime = core::str::from_utf8(bytes.get(at + 1..at + 1 + mime_len)?).ok()?;
        Some((
            Self {
                hash,
                name: name.to_string(),
                mime: mime.to_string(),
            },
            at + 1 + mime_len,
        ))
    }

    /// The name as something safe to write to a filesystem or hand to a
    /// download attribute. The wire name crossed an untrusted channel: this
    /// strips any path, drops control characters, and refuses the handful of
    /// names that would traverse, hide, or vanish.
    pub fn safe_name(&self) -> String {
        let base = self.name.rsplit(['/', '\\']).next().unwrap_or("");
        let clean: String = base.chars().filter(|c| !c.is_control()).collect();
        let clean = clean.trim();
        if clean.is_empty() || clean == "." || clean == ".." {
            return "received.bin".to_string();
        }
        if clean.starts_with('.') {
            // A dotfile arriving over an air gap is not a hidden file the
            // receiver asked for.
            return format!("_{clean}");
        }
        clean.to_string()
    }
}

/// Truncate to at most `max` bytes without splitting a UTF-8 character.
fn truncated(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_the_wire_form() {
        let manifest = Manifest::describe("cuttlefish.pdf", "application/pdf", b"ink");
        let bytes = manifest.to_bytes();
        let (parsed, used) = Manifest::parse(&bytes).unwrap();
        assert_eq!(parsed, manifest);
        assert_eq!(used, bytes.len());
        // Trailing garbage after the manifest must not confuse the parse.
        let mut padded = bytes.clone();
        padded.extend_from_slice(&[0xAA; 7]);
        assert_eq!(Manifest::parse(&padded).unwrap().1, bytes.len());
    }

    #[test]
    fn hash_is_blake3_of_the_object() {
        let object = b"strobe";
        let manifest = Manifest::describe("f", "", object);
        assert_eq!(&manifest.hash, blake3::hash(object).as_bytes());
    }

    /// Truncation must respect character boundaries, or a multibyte name would
    /// produce invalid UTF-8 and the far end would reject its own manifest.
    #[test]
    fn long_names_truncate_on_char_boundaries() {
        let name = "烏賊".repeat(40); // 3 bytes per char, 240 bytes
        let manifest = Manifest::describe(&name, "", &[]);
        assert!(manifest.name.len() <= MAX_NAME);
        assert!(manifest.name.chars().all(|c| c == '烏' || c == '賊'));
        let bytes = manifest.to_bytes();
        assert_eq!(Manifest::parse(&bytes).unwrap().0, manifest);
    }

    #[test]
    fn oversized_fields_on_the_wire_are_rejected() {
        let mut bytes = vec![0u8; HASH_LEN];
        bytes.push((MAX_NAME + 1) as u8);
        bytes.extend_from_slice(&[b'a'; MAX_NAME + 1]);
        bytes.push(0);
        assert!(Manifest::parse(&bytes).is_none());
    }

    #[test]
    fn safe_name_defuses_hostile_names() {
        let name = |n: &str| Manifest {
            hash: [0; HASH_LEN],
            name: n.to_string(),
            mime: String::new(),
        };
        assert_eq!(name("../../etc/passwd").safe_name(), "passwd");
        assert_eq!(name("..\\..\\boot.ini").safe_name(), "boot.ini");
        assert_eq!(name("").safe_name(), "received.bin");
        assert_eq!(name("..").safe_name(), "received.bin");
        assert_eq!(name(".bashrc").safe_name(), "_.bashrc");
        assert_eq!(name("a\u{0}b\nc.txt").safe_name(), "abc.txt");
        assert_eq!(name("plain.txt").safe_name(), "plain.txt");
    }
}
