//! Independent, uncached, read-only temperature candidates. No control verification.
use crate::{transport::TemperatureTransport, AdapterError, Identity, Metadata};
use serde::Serialize;

pub(crate) trait ReadIo {
    fn identity(&mut self) -> Result<Identity, AdapterError>;
    fn info(&mut self, key: &str) -> Result<Metadata, AdapterError>;
    fn read(&mut self, key: &str, metadata: Metadata) -> Result<Vec<u8>, AdapterError>;
}

pub(crate) fn key_code(key: &str) -> Result<u32, AdapterError> {
    let bytes = key.as_bytes();
    if bytes.len() != 4 || bytes[0] != b'T' || !bytes.iter().all(u8::is_ascii_graphic) {
        return Err(malformed(
            key,
            "temperature key requires four ASCII graphic bytes starting with T",
        ));
    }
    Ok(u32::from_be_bytes(
        bytes.try_into().expect("validated length"),
    ))
}
fn malformed(key: &str, reason: &str) -> AdapterError {
    AdapterError::Malformed {
        key: key.into(),
        reason: reason.into(),
    }
}
fn validate_metadata(key: &str, metadata: Metadata) -> Result<(), AdapterError> {
    match (metadata.kind, metadata.size) {
        (kind, 4) if kind == *b"flt " => Ok(()),
        (kind, 2) if kind == *b"sp78" => Ok(()),
        _ => Err(malformed(
            key,
            "temperature requires exactly flt /4 or sp78/2",
        )),
    }
}
fn decode(key: &str, metadata: Metadata, bytes: &[u8]) -> Result<f64, AdapterError> {
    validate_metadata(key, metadata)?;
    if bytes.len() != metadata.size as usize {
        return Err(malformed(key, "temperature size mismatch"));
    }
    let value = if metadata.kind == *b"flt " {
        f32::from_le_bytes(bytes.try_into().expect("validated size")) as f64
    } else {
        i16::from_be_bytes(bytes.try_into().expect("validated size")) as f64 / 256.0
    };
    if !value.is_finite() {
        return Err(malformed(key, "nonfinite temperature"));
    }
    if !(-40.0..=150.0).contains(&value) {
        return Err(malformed(
            key,
            "temperature outside candidate range -40..150 C",
        ));
    }
    Ok(value)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemperatureReading {
    pub identity: Identity,
    pub key: String,
    pub metadata: Metadata,
    /// A protocol-decoded candidate, never a calibrated/control-verified sensor.
    pub candidate_celsius: f64,
    /// Zero is preserved, but marked suspect rather than silently discarded.
    pub suspect: bool,
}

struct Reader<I> {
    io: I,
    identity: Identity,
}
impl<I: ReadIo> Reader<I> {
    fn new(mut io: I) -> Result<Self, AdapterError> {
        let identity = io.identity()?;
        Ok(Self { io, identity })
    }
    fn check_identity(&mut self) -> Result<(), AdapterError> {
        if self.io.identity()? != self.identity {
            return Err(AdapterError::IdentityChanged);
        }
        Ok(())
    }
    fn read(&mut self, key: &str) -> Result<TemperatureReading, AdapterError> {
        key_code(key)?;
        self.check_identity()?;
        let metadata = self.io.info(key)?;
        validate_metadata(key, metadata)?;
        let bytes = self.io.read(key, metadata)?;
        if self.io.info(key)? != metadata {
            return Err(AdapterError::MetadataChanged { key: key.into() });
        }
        self.check_identity()?;
        let candidate_celsius = decode(key, metadata, &bytes)?;
        Ok(TemperatureReading {
            identity: self.identity.clone(),
            key: key.into(),
            metadata,
            candidate_celsius,
            suspect: candidate_celsius == 0.0,
        })
    }
}

/// Owns a separate AppleSMC user-client connection; never acquires actuator locks.
/// No sensor name mapping, fallback, cached reading, or write method is provided.
pub struct TemperatureReader {
    inner: Reader<TemperatureTransport>,
}
impl TemperatureReader {
    pub fn open() -> Result<Self, AdapterError> {
        Ok(Self {
            inner: Reader::new(TemperatureTransport::open()?)?,
        })
    }
    pub fn read(&mut self, key: &str) -> Result<TemperatureReading, AdapterError> {
        self.inner.read(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    fn float() -> Metadata {
        Metadata {
            kind: *b"flt ",
            size: 4,
            attributes: 0,
        }
    }
    fn fixed() -> Metadata {
        Metadata {
            kind: *b"sp78",
            size: 2,
            attributes: 0,
        }
    }
    fn identity() -> Identity {
        Identity {
            model: "test".into(),
            architecture: "test".into(),
        }
    }
    #[test]
    fn key_validation() {
        assert_eq!(key_code("Tp01").unwrap(), u32::from_be_bytes(*b"Tp01"));
        for key in [
            "",
            "T",
            "Tp001",
            "F0Tg",
            "tp01",
            "T p1",
            "T\n01",
            "Té1",
            "T\u{7f}01",
        ] {
            assert!(key_code(key).is_err(), "{key:?}");
        }
    }
    #[test]
    fn exact_encoding_sizes_endianness_and_signed_values() {
        assert_eq!(
            decode("Tp01", float(), &42.25f32.to_le_bytes()).unwrap(),
            42.25
        );
        assert_eq!(decode("Tp01", fixed(), &[0xff, 0x80]).unwrap(), -0.5);
        for size in [0, 1, 2, 3, 5, 32, u32::MAX] {
            let mut m = float();
            m.size = size;
            assert!(decode("Tp01", m, &[0; 4]).is_err());
        }
        let mut m = fixed();
        m.size = 4;
        assert!(decode("Tp01", m, &[0; 4]).is_err());
        let mut m = float();
        m.kind = *b"ui32";
        assert!(decode("Tp01", m, &[0; 4]).is_err());
        for len in [0, 1, 3, 5, 32] {
            assert!(decode("Tp01", float(), &vec![0; len]).is_err());
        }
    }
    #[test]
    fn finite_range_boundaries_and_zero() {
        for value in [-40.0f32, 0.0, -0.0, 150.0] {
            assert_eq!(
                decode("Tp01", float(), &value.to_le_bytes()).unwrap(),
                value as f64
            );
        }
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -40.01, 150.01] {
            assert!(decode("Tp01", float(), &value.to_le_bytes()).is_err());
        }
    }
    struct Fake {
        identities: VecDeque<Result<Identity, AdapterError>>,
        infos: VecDeque<Result<Metadata, AdapterError>>,
        reads: VecDeque<Result<Vec<u8>, AdapterError>>,
        calls: Vec<&'static str>,
    }
    impl ReadIo for Fake {
        fn identity(&mut self) -> Result<Identity, AdapterError> {
            self.calls.push("identity");
            self.identities
                .pop_front()
                .unwrap_or_else(|| Ok(identity()))
        }
        fn info(&mut self, _: &str) -> Result<Metadata, AdapterError> {
            self.calls.push("info");
            self.infos.pop_front().unwrap_or_else(|| Ok(float()))
        }
        fn read(&mut self, _: &str, _: Metadata) -> Result<Vec<u8>, AdapterError> {
            self.calls.push("read");
            self.reads
                .pop_front()
                .unwrap_or_else(|| Ok(0f32.to_le_bytes().to_vec()))
        }
    }
    fn fake() -> Fake {
        Fake {
            identities: VecDeque::new(),
            infos: VecDeque::new(),
            reads: VecDeque::new(),
            calls: vec![],
        }
    }
    #[test]
    fn fresh_read_only_io_and_zero_preserved() {
        let mut reader = Reader::new(fake()).unwrap();
        assert!(reader.read("Tp01").unwrap().suspect);
        reader.io.reads.push_back(Err(AdapterError::Timeout));
        assert_eq!(reader.read("Tp01").unwrap_err(), AdapterError::Timeout);
        assert_eq!(
            reader.io.calls,
            [
                "identity", "identity", "info", "read", "info", "identity", "identity", "info",
                "read"
            ]
        );
        // ReadIo has no write operation; fake dispatch records every operation.
    }
    #[test]
    fn changes_and_failures_are_not_success() {
        let mut io = fake();
        io.infos.extend([Ok(float()), Ok(fixed())]);
        assert!(matches!(
            Reader::new(io).unwrap().read("Tp01"),
            Err(AdapterError::MetadataChanged { .. })
        ));
        for changed_at in [1, 2] {
            let mut io = fake();
            io.identities.extend((0..=changed_at).map(|index| {
                let mut id = identity();
                if index == changed_at {
                    id.model = "changed".into();
                }
                Ok(id)
            }));
            assert_eq!(
                Reader::new(io).unwrap().read("Tp01").unwrap_err(),
                AdapterError::IdentityChanged
            );
        }
        let mut reader = Reader::new(fake()).unwrap();
        assert!(reader.read("F0Tg").is_err());
        assert_eq!(reader.io.calls, ["identity"]);
        reader
            .io
            .infos
            .push_back(Err(AdapterError::MissingKey { key: "Tp01".into() }));
        assert!(matches!(
            reader.read("Tp01"),
            Err(AdapterError::MissingKey { .. })
        ));
    }
}
