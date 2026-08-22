use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::OpenOptions;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::ast::{LeanEnvironmentIdentity, LeanName};
use crate::worker::{LeanWorker, WorkerError};

const MAGIC: &[u8; 8] = b"RFLCAT03";
const CHECKSUM_BYTES: usize = 32;

#[derive(Debug)]
pub enum CatalogError {
    Io(std::io::Error),
    Worker(WorkerError),
    Invalid(String),
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "Lean catalog I/O failed: {error}"),
            Self::Worker(error) => error.fmt(formatter),
            Self::Invalid(error) => write!(formatter, "invalid Lean catalog: {error}"),
        }
    }
}

impl std::error::Error for CatalogError {}

impl From<std::io::Error> for CatalogError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<WorkerError> for CatalogError {
    fn from(error: WorkerError) -> Self {
        Self::Worker(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum NameComponent {
    Anonymous,
    String { parent: u32, value: String },
    Number { parent: u32, value: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeclarationKind {
    Axiom,
    Definition,
    Theorem,
    Opaque,
    Quotient,
    Inductive,
    Constructor,
    Recursor,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogEntry {
    pub name: u32,
    pub module_name: u32,
    pub statement_hash: u64,
    pub dependencies: Vec<u32>,
    pub kind: DeclarationKind,
    pub locally_eligible: bool,
    pub eligible: bool,
}

#[derive(Clone, Debug)]
pub struct LeanCatalog {
    environment: LeanEnvironmentIdentity,
    names: Vec<NameComponent>,
    entries: Vec<CatalogEntry>,
    content_sha256: String,
}

impl LeanCatalog {
    pub fn build(worker: &LeanWorker, page_size: usize) -> Result<Self, CatalogError> {
        if page_size == 0 {
            return Err(CatalogError::Invalid("page size must be positive".into()));
        }
        let mut names = Vec::new();
        let mut name_ids = HashMap::new();
        let anonymous = intern_name(&LeanName::Anonymous, &mut names, &mut name_ids)?;
        if anonymous != 0 {
            return Err(CatalogError::Invalid(
                "anonymous name did not receive symbol zero".into(),
            ));
        }
        let mut entries = Vec::new();
        let mut offset = 0;
        let mut total = usize::MAX;
        while offset < total {
            let page = worker.fingerprint_page(offset, page_size)?;
            if page.offset != offset || page.fingerprints.is_empty() && page.offset < page.total {
                return Err(CatalogError::Invalid(
                    "worker returned a non-progressing catalog page".into(),
                ));
            }
            total = page.total;
            for fingerprint in page.fingerprints {
                let name = intern_name(&fingerprint.name, &mut names, &mut name_ids)?;
                let module_name = intern_name(&fingerprint.module_name, &mut names, &mut name_ids)?;
                let statement_hash = fingerprint.statement_hash.parse().map_err(|_| {
                    CatalogError::Invalid("statement hash is not an unsigned 64-bit value".into())
                })?;
                let dependencies = fingerprint
                    .dependencies
                    .iter()
                    .map(|dependency| intern_name(dependency, &mut names, &mut name_ids))
                    .collect::<Result<Vec<_>, _>>()?;
                entries.push(CatalogEntry {
                    name,
                    module_name,
                    statement_hash,
                    dependencies,
                    kind: DeclarationKind::parse(&fingerprint.kind)?,
                    locally_eligible: fingerprint.locally_eligible,
                    eligible: fingerprint.locally_eligible,
                });
            }
            offset = offset.saturating_add(page_size).min(total);
        }
        if entries.len() != total {
            return Err(CatalogError::Invalid(format!(
                "catalog contains {} entries, expected {total}",
                entries.len()
            )));
        }
        Self::from_parts(worker.environment().clone(), names, entries)
    }

    pub fn load(path: &Path) -> Result<Self, CatalogError> {
        let bytes = std::fs::read(path)?;
        if bytes.len() < MAGIC.len() + CHECKSUM_BYTES {
            return Err(CatalogError::Invalid("catalog is truncated".into()));
        }
        let payload_len = bytes.len() - CHECKSUM_BYTES;
        let (payload, expected_digest) = bytes.split_at(payload_len);
        if Sha256::digest(payload).as_slice() != expected_digest {
            return Err(CatalogError::Invalid("content digest differs".into()));
        }
        let mut decoder = Decoder::new(payload);
        decoder.expect(MAGIC)?;
        let environment = LeanEnvironmentIdentity {
            mathlib_commit: decoder.string()?,
            lean_toolchain: decoder.string()?,
            lean_commit: decoder.string()?,
            artifact_format: decoder.u32()?,
            kernel_contract: decoder.u32()?,
            worker_source_sha256: decoder.string()?,
        };
        let name_count = decoder.usize()?;
        let mut names = Vec::with_capacity(name_count);
        for index in 0..name_count {
            let component = match decoder.byte()? {
                0 if index == 0 => NameComponent::Anonymous,
                1 => NameComponent::String {
                    parent: decoder.name_id(index)?,
                    value: decoder.string()?,
                },
                2 => NameComponent::Number {
                    parent: decoder.name_id(index)?,
                    value: decoder.usize()?,
                },
                _ => {
                    return Err(CatalogError::Invalid(
                        "name tag or anonymous position differs".into(),
                    ));
                }
            };
            names.push(component);
        }
        let entry_count = decoder.usize()?;
        let mut entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            let name = decoder.name_id(name_count)?;
            let module_name = decoder.name_id(name_count)?;
            let statement_hash = decoder.u64()?;
            let dependency_count = decoder.usize()?;
            let mut dependencies = Vec::with_capacity(dependency_count);
            for _ in 0..dependency_count {
                dependencies.push(decoder.name_id(name_count)?);
            }
            entries.push(CatalogEntry {
                name,
                module_name,
                statement_hash,
                dependencies,
                kind: DeclarationKind::decode(decoder.byte()?)?,
                locally_eligible: decoder.boolean()?,
                eligible: false,
            });
        }
        if !decoder.is_finished() {
            return Err(CatalogError::Invalid(
                "catalog has trailing payload bytes".into(),
            ));
        }
        let content_sha256 = hex(expected_digest);
        resolve_eligibility(&mut entries)?;
        Ok(Self {
            environment,
            names,
            entries,
            content_sha256,
        })
    }

    pub fn save_new(&self, path: &Path) -> Result<(), CatalogError> {
        if path.exists() {
            return Err(CatalogError::Invalid(format!(
                "catalog already exists: {}",
                path.display()
            )));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let bytes = self.encode()?;
        let temporary = temporary_path(path);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let mut output = BufWriter::new(file);
        output.write_all(&bytes)?;
        output.flush()?;
        output.get_ref().sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    }

    #[must_use]
    pub fn environment(&self) -> &LeanEnvironmentIdentity {
        &self.environment
    }

    #[must_use]
    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    pub fn eligible_entries(&self) -> impl Iterator<Item = &CatalogEntry> {
        self.entries.iter().filter(|entry| entry.eligible)
    }

    pub fn require_environment(
        &self,
        expected: &LeanEnvironmentIdentity,
    ) -> Result<(), CatalogError> {
        if &self.environment == expected {
            Ok(())
        } else {
            Err(CatalogError::Invalid(
                "catalog environment identity differs from the installed Lean domain".into(),
            ))
        }
    }

    #[must_use]
    pub fn name(&self, id: u32) -> Option<LeanName> {
        expand_name(&self.names, id)
    }

    #[must_use]
    pub fn content_sha256(&self) -> &str {
        &self.content_sha256
    }

    fn from_parts(
        environment: LeanEnvironmentIdentity,
        names: Vec<NameComponent>,
        mut entries: Vec<CatalogEntry>,
    ) -> Result<Self, CatalogError> {
        resolve_eligibility(&mut entries)?;
        let mut catalog = Self {
            environment,
            names,
            entries,
            content_sha256: String::new(),
        };
        let payload = catalog.encode_payload()?;
        catalog.content_sha256 = hex(&Sha256::digest(payload));
        Ok(catalog)
    }

    fn encode(&self) -> Result<Vec<u8>, CatalogError> {
        let mut payload = self.encode_payload()?;
        let digest = Sha256::digest(&payload);
        if hex(&digest) != self.content_sha256 {
            return Err(CatalogError::Invalid(
                "in-memory catalog content digest differs".into(),
            ));
        }
        payload.extend_from_slice(&digest);
        Ok(payload)
    }

    fn encode_payload(&self) -> Result<Vec<u8>, CatalogError> {
        let mut output = Vec::new();
        output.extend_from_slice(MAGIC);
        write_string(&mut output, &self.environment.mathlib_commit);
        write_string(&mut output, &self.environment.lean_toolchain);
        write_string(&mut output, &self.environment.lean_commit);
        write_varint(&mut output, u64::from(self.environment.artifact_format));
        write_varint(&mut output, u64::from(self.environment.kernel_contract));
        write_string(&mut output, &self.environment.worker_source_sha256);
        write_usize(&mut output, self.names.len())?;
        for component in &self.names {
            match component {
                NameComponent::Anonymous => output.push(0),
                NameComponent::String { parent, value } => {
                    output.push(1);
                    write_varint(&mut output, u64::from(*parent));
                    write_string(&mut output, value);
                }
                NameComponent::Number { parent, value } => {
                    output.push(2);
                    write_varint(&mut output, u64::from(*parent));
                    write_usize(&mut output, *value)?;
                }
            }
        }
        write_usize(&mut output, self.entries.len())?;
        for entry in &self.entries {
            write_varint(&mut output, u64::from(entry.name));
            write_varint(&mut output, u64::from(entry.module_name));
            output.extend_from_slice(&entry.statement_hash.to_le_bytes());
            write_usize(&mut output, entry.dependencies.len())?;
            for dependency in &entry.dependencies {
                write_varint(&mut output, u64::from(*dependency));
            }
            output.push(entry.kind.encode());
            output.push(u8::from(entry.locally_eligible));
        }
        Ok(output)
    }
}

impl DeclarationKind {
    fn parse(value: &str) -> Result<Self, CatalogError> {
        match value {
            "axiom" => Ok(Self::Axiom),
            "definition" => Ok(Self::Definition),
            "theorem" => Ok(Self::Theorem),
            "opaque" => Ok(Self::Opaque),
            "quotient" => Ok(Self::Quotient),
            "inductive" => Ok(Self::Inductive),
            "constructor" => Ok(Self::Constructor),
            "recursor" => Ok(Self::Recursor),
            _ => Err(CatalogError::Invalid(format!(
                "unknown declaration kind {value}"
            ))),
        }
    }

    #[must_use]
    pub const fn code(&self) -> u8 {
        self.encode()
    }

    const fn encode(&self) -> u8 {
        match self {
            Self::Axiom => 0,
            Self::Definition => 1,
            Self::Theorem => 2,
            Self::Opaque => 3,
            Self::Quotient => 4,
            Self::Inductive => 5,
            Self::Constructor => 6,
            Self::Recursor => 7,
        }
    }

    fn decode(value: u8) -> Result<Self, CatalogError> {
        match value {
            0 => Ok(Self::Axiom),
            1 => Ok(Self::Definition),
            2 => Ok(Self::Theorem),
            3 => Ok(Self::Opaque),
            4 => Ok(Self::Quotient),
            5 => Ok(Self::Inductive),
            6 => Ok(Self::Constructor),
            7 => Ok(Self::Recursor),
            _ => Err(CatalogError::Invalid("declaration kind tag differs".into())),
        }
    }
}

fn resolve_eligibility(entries: &mut [CatalogEntry]) -> Result<(), CatalogError> {
    let indexes = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.name, index))
        .collect::<HashMap<_, _>>();
    if indexes.len() != entries.len() {
        return Err(CatalogError::Invalid(
            "catalog contains duplicate declaration names".into(),
        ));
    }
    for entry in entries.iter_mut() {
        entry.eligible = entry.locally_eligible
            && entry
                .dependencies
                .iter()
                .all(|dependency| indexes.contains_key(dependency));
    }
    loop {
        let previous = entries
            .iter()
            .map(|entry| entry.eligible)
            .collect::<Vec<_>>();
        let mut changed = false;
        for entry in entries.iter_mut().filter(|entry| entry.eligible) {
            if entry.dependencies.iter().any(|dependency| {
                indexes
                    .get(dependency)
                    .is_none_or(|index| !previous[*index])
            }) {
                entry.eligible = false;
                changed = true;
            }
        }
        if !changed {
            return Ok(());
        }
    }
}

fn expand_name(names: &[NameComponent], id: u32) -> Option<LeanName> {
    match names.get(id as usize)? {
        NameComponent::Anonymous => Some(LeanName::Anonymous),
        NameComponent::String { parent, value } => Some(LeanName::Str {
            parent: Box::new(expand_name(names, *parent)?),
            value: value.clone(),
        }),
        NameComponent::Number { parent, value } => Some(LeanName::Num {
            parent: Box::new(expand_name(names, *parent)?),
            value: *value,
        }),
    }
}

fn intern_name(
    name: &LeanName,
    names: &mut Vec<NameComponent>,
    ids: &mut HashMap<LeanName, u32>,
) -> Result<u32, CatalogError> {
    if let Some(id) = ids.get(name) {
        return Ok(*id);
    }
    let component = match name {
        LeanName::Anonymous => NameComponent::Anonymous,
        LeanName::Str { parent, value } => NameComponent::String {
            parent: intern_name(parent, names, ids)?,
            value: value.clone(),
        },
        LeanName::Num { parent, value } => NameComponent::Number {
            parent: intern_name(parent, names, ids)?,
            value: *value,
        },
    };
    let id = u32::try_from(names.len())
        .map_err(|_| CatalogError::Invalid("catalog exceeds u32 symbol IDs".into()))?;
    names.push(component);
    ids.insert(name.clone(), id);
    Ok(id)
}

fn write_string(output: &mut Vec<u8>, value: &str) {
    write_varint(output, value.len() as u64);
    output.extend_from_slice(value.as_bytes());
}

fn write_usize(output: &mut Vec<u8>, value: usize) -> Result<(), CatalogError> {
    let value = u64::try_from(value)
        .map_err(|_| CatalogError::Invalid("usize exceeds canonical u64 encoding".into()))?;
    write_varint(output, value);
    Ok(())
}

fn write_varint(output: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        output.push((value.to_le_bytes()[0] & 0x7f) | 0x80);
        value >>= 7;
    }
    output.push(value.to_le_bytes()[0]);
}

struct Decoder<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    const fn new(input: &'a [u8]) -> Self {
        Self { input, offset: 0 }
    }

    fn expect(&mut self, expected: &[u8]) -> Result<(), CatalogError> {
        if self.take(expected.len())? != expected {
            return Err(CatalogError::Invalid("catalog magic differs".into()));
        }
        Ok(())
    }

    fn byte(&mut self) -> Result<u8, CatalogError> {
        Ok(self.take(1)?[0])
    }

    fn u64(&mut self) -> Result<u64, CatalogError> {
        let encoded: [u8; 8] = self
            .take(8)?
            .try_into()
            .map_err(|_| CatalogError::Invalid("u64 width differs".into()))?;
        Ok(u64::from_le_bytes(encoded))
    }

    fn varint(&mut self) -> Result<u64, CatalogError> {
        let mut value = 0_u64;
        for shift in (0..=63).step_by(7) {
            let byte = self.byte()?;
            let payload = u64::from(byte & 0x7f);
            if shift == 63 && payload > 1 {
                return Err(CatalogError::Invalid("varint overflows u64".into()));
            }
            value |= payload << shift;
            if byte & 0x80 == 0 {
                if shift != 0 && payload == 0 {
                    return Err(CatalogError::Invalid("varint is not minimal".into()));
                }
                return Ok(value);
            }
        }
        Err(CatalogError::Invalid("varint is unterminated".into()))
    }

    fn usize(&mut self) -> Result<usize, CatalogError> {
        usize::try_from(self.varint()?)
            .map_err(|_| CatalogError::Invalid("encoded value exceeds usize".into()))
    }

    fn u32(&mut self) -> Result<u32, CatalogError> {
        u32::try_from(self.varint()?)
            .map_err(|_| CatalogError::Invalid("integer exceeds u32".into()))
    }

    fn boolean(&mut self) -> Result<bool, CatalogError> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(CatalogError::Invalid("boolean tag differs".into())),
        }
    }

    fn name_id(&mut self, upper_bound: usize) -> Result<u32, CatalogError> {
        let id = u32::try_from(self.varint()?)
            .map_err(|_| CatalogError::Invalid("name ID exceeds u32".into()))?;
        if id as usize >= upper_bound {
            return Err(CatalogError::Invalid("name ID is out of range".into()));
        }
        Ok(id)
    }

    fn string(&mut self) -> Result<String, CatalogError> {
        let length = self.usize()?;
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| CatalogError::Invalid("string is not UTF-8".into()))
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], CatalogError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| CatalogError::Invalid("offset overflowed".into()))?;
        let bytes = self
            .input
            .get(self.offset..end)
            .ok_or_else(|| CatalogError::Invalid("catalog is truncated".into()))?;
        self.offset = end;
        Ok(bytes)
    }

    fn is_finished(&self) -> bool {
        self.offset == self.input.len()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
            hex
        })
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".tmp-{}", std::process::id()));
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::ast::{LeanEnvironmentIdentity, LeanName};

    use super::{
        CatalogEntry, DeclarationKind, Decoder, LeanCatalog, NameComponent, intern_name,
        write_varint,
    };

    #[test]
    fn canonical_varints_round_trip_boundaries() {
        for expected in [0, 1, 127, 128, 16_383, 16_384, u64::MAX] {
            let mut encoded = Vec::new();
            write_varint(&mut encoded, expected);
            let mut decoder = Decoder::new(&encoded);
            assert_eq!(decoder.varint().unwrap(), expected);
            assert!(decoder.is_finished());
        }
    }

    #[test]
    fn overlong_varints_are_rejected() {
        let mut decoder = Decoder::new(&[0x80, 0]);
        assert!(decoder.varint().is_err());
    }

    #[test]
    fn eligibility_is_dependency_closed_and_binary_round_trip_is_checked() {
        let mut names = Vec::<NameComponent>::new();
        let mut ids = HashMap::new();
        let _ = intern_name(&LeanName::Anonymous, &mut names, &mut ids).unwrap();
        let a = intern_name(&LeanName::from_dotted("A"), &mut names, &mut ids).unwrap();
        let b = intern_name(&LeanName::from_dotted("B"), &mut names, &mut ids).unwrap();
        let c = intern_name(&LeanName::from_dotted("C"), &mut names, &mut ids).unwrap();
        let environment = LeanEnvironmentIdentity {
            mathlib_commit: "mathlib".into(),
            lean_toolchain: "toolchain".into(),
            lean_commit: "lean".into(),
            artifact_format: 2,
            kernel_contract: 2,
            worker_source_sha256: "worker".into(),
        };
        let entry = |name, dependencies, locally_eligible| CatalogEntry {
            name,
            module_name: name,
            statement_hash: u64::from(name),
            dependencies,
            kind: DeclarationKind::Theorem,
            locally_eligible,
            eligible: locally_eligible,
        };
        let catalog = LeanCatalog::from_parts(
            environment.clone(),
            names,
            vec![
                entry(a, vec![b], true),
                entry(b, vec![], false),
                entry(c, vec![], true),
            ],
        )
        .unwrap();
        assert!(!catalog.entries()[0].eligible);
        assert!(!catalog.entries()[1].eligible);
        assert!(catalog.entries()[2].eligible);

        let path = std::env::temp_dir().join(format!(
            "reflex-lean-catalog-test-{}-{}.bin",
            std::process::id(),
            catalog.content_sha256()
        ));
        let corrupt = path.with_extension("corrupt.bin");
        catalog.save_new(&path).unwrap();
        let loaded = LeanCatalog::load(&path).unwrap();
        assert_eq!(loaded.environment(), &environment);
        assert_eq!(loaded.entries(), catalog.entries());
        assert_eq!(loaded.content_sha256(), catalog.content_sha256());
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[8] ^= 1;
        std::fs::write(&corrupt, bytes).unwrap();
        assert!(LeanCatalog::load(&corrupt).is_err());
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(corrupt).unwrap();
    }
}
