//! Read-only host registry snapshots used by direct-folder launches.

use crate::process::{Process, RegistryKeyMetadata};
use std::collections::{HashMap, HashSet};

const MAGIC: &[u8; 8] = b"AWREG1\0\0";
const MAX_KEYS: usize = 4096;
const MAX_ITEMS: usize = 65_536;
const MAX_TEXT: usize = 1 << 20;
const MAX_VALUE_BYTES: usize = 16 << 20;
const SIMS_REGISTRY_ROOT: &str = "hkey_local_machine\\software\\sims(steam)";
const EA_SIMS_REGISTRY_ROOT: &str = "hkey_local_machine\\software\\electronic arts\\sims(steam)";
const HOST_CPU_REGISTRY_ROOT: &str =
    "hkey_local_machine\\hardware\\description\\system\\centralprocessor\\0";
const HOST_REGISTRY_ROOTS: [&str; 3] = [
    SIMS_REGISTRY_ROOT,
    EA_SIMS_REGISTRY_ROOT,
    HOST_CPU_REGISTRY_ROOT,
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryValueRecord {
    pub name: String,
    pub value_type: u32,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryKeyRecord {
    pub path: String,
    pub class_name: String,
    pub security_descriptor_size: u32,
    pub last_write_filetime: u64,
    pub subkeys: Vec<String>,
    pub values: Vec<RegistryValueRecord>,
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| "registry snapshot length overflow".to_owned())?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or_else(|| "registry snapshot is truncated".to_owned())?;
        self.offset = end;
        Ok(bytes)
    }

    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn text(&mut self) -> Result<String, String> {
        let length = self.u32()? as usize;
        if length > MAX_TEXT {
            return Err("registry snapshot text field exceeds the limit".to_owned());
        }
        let text = std::str::from_utf8(self.take(length)?)
            .map_err(|_| "registry snapshot contains invalid UTF-8".to_owned())?;
        if text.contains('\0') {
            return Err("registry snapshot text contains an embedded NUL".to_owned());
        }
        Ok(text.to_owned())
    }
}

fn validate_component(component: &str) -> bool {
    !component.is_empty()
        && component != "."
        && component != ".."
        && !component.contains(['\\', '/', ':', '\0'])
}

fn normalize_path(path: &str) -> Result<String, String> {
    if path.starts_with(['\\', '/']) || path.contains(':') || path.contains('\0') {
        return Err("registry snapshot contains an absolute or invalid key path".to_owned());
    }
    let mut components = path.split('\\');
    let first = components
        .next()
        .ok_or_else(|| "registry snapshot contains an empty key path".to_owned())?;
    if !validate_component(first) || components.clone().any(|part| !validate_component(part)) {
        return Err("registry snapshot contains an invalid key path component".to_owned());
    }
    Ok(path.to_lowercase())
}

pub fn parse_host_snapshot(bytes: &[u8]) -> Result<Vec<RegistryKeyRecord>, String> {
    let mut reader = Reader { bytes, offset: 0 };
    if reader.take(MAGIC.len())? != MAGIC {
        return Err("registry snapshot has an invalid signature".to_owned());
    }
    let key_count = reader.u32()? as usize;
    if key_count == 0 || key_count > MAX_KEYS {
        return Err("registry snapshot key count is outside the accepted range".to_owned());
    }

    let mut records = Vec::with_capacity(key_count);
    let mut by_path = HashMap::with_capacity(key_count);
    for _ in 0..key_count {
        let path = reader.text()?;
        let normalized = normalize_path(&path)?;
        if !HOST_REGISTRY_ROOTS
            .iter()
            .any(|root| normalized == *root || normalized.starts_with(&format!("{root}\\")))
        {
            return Err(format!(
                "registry snapshot key is outside the supplied host roots: {path}"
            ));
        }
        if by_path.insert(normalized, records.len()).is_some() {
            return Err(format!("registry snapshot repeats key path: {path}"));
        }

        let class_name = reader.text()?;
        let security_descriptor_size = reader.u32()?;
        let last_write_filetime = reader.u64()?;
        let subkey_count = reader.u32()? as usize;
        if subkey_count > MAX_ITEMS {
            return Err("registry snapshot subkey count exceeds the limit".to_owned());
        }
        let mut subkeys = Vec::with_capacity(subkey_count);
        let mut normalized_subkeys = HashSet::with_capacity(subkey_count);
        for _ in 0..subkey_count {
            let subkey = reader.text()?;
            if !validate_component(&subkey) || !normalized_subkeys.insert(subkey.to_lowercase()) {
                return Err(format!(
                    "registry snapshot has an invalid or duplicate subkey under {path}"
                ));
            }
            subkeys.push(subkey);
        }

        let value_count = reader.u32()? as usize;
        if value_count > MAX_ITEMS {
            return Err("registry snapshot value count exceeds the limit".to_owned());
        }
        let mut values = Vec::with_capacity(value_count);
        let mut normalized_values = HashSet::with_capacity(value_count);
        for _ in 0..value_count {
            let name = reader.text()?;
            if !normalized_values.insert(name.to_lowercase()) {
                return Err(format!(
                    "registry snapshot repeats value {name:?} under {path}"
                ));
            }
            let value_type = reader.u32()?;
            let data_length = reader.u32()? as usize;
            if data_length > MAX_VALUE_BYTES {
                return Err("registry snapshot value data exceeds the limit".to_owned());
            }
            values.push(RegistryValueRecord {
                name,
                value_type,
                data: reader.take(data_length)?.to_vec(),
            });
        }

        records.push(RegistryKeyRecord {
            path,
            class_name,
            security_descriptor_size,
            last_write_filetime,
            subkeys,
            values,
        });
    }
    if reader.offset != bytes.len() {
        return Err("registry snapshot has trailing bytes".to_owned());
    }

    let record_paths = by_path.keys().cloned().collect::<HashSet<_>>();
    let root_present = record_paths.contains(SIMS_REGISTRY_ROOT);
    if !root_present {
        return Err("registry snapshot omits the Sims registration root".to_owned());
    }
    if !record_paths.contains(EA_SIMS_REGISTRY_ROOT) {
        return Err(
            "registry snapshot omits the Electronic Arts Sims registration root".to_owned(),
        );
    }
    for record in &records {
        let parent_path = normalize_path(&record.path)?;
        let declared = record
            .subkeys
            .iter()
            .map(|name| format!("{parent_path}\\{}", name.to_lowercase()))
            .collect::<HashSet<_>>();
        let actual = record_paths
            .iter()
            .filter_map(|candidate| {
                let (parent, leaf) = candidate.rsplit_once('\\')?;
                (parent == parent_path).then(|| format!("{parent}\\{leaf}"))
            })
            .collect::<HashSet<_>>();
        if declared != actual {
            return Err(format!(
                "registry snapshot subkey list does not match key records for {}",
                record.path
            ));
        }
        if !HOST_REGISTRY_ROOTS.contains(&parent_path.as_str()) {
            let parent = parent_path
                .rsplit_once('\\')
                .map(|(parent, _)| parent)
                .ok_or_else(|| "registry snapshot key has no parent".to_owned())?;
            if !record_paths.contains(parent) {
                return Err(format!(
                    "registry snapshot omits the parent of {}",
                    record.path
                ));
            }
        }
    }
    Ok(records)
}

/// Installs a validated, read-only snapshot of the active Sims registry registration.
pub fn load_host_snapshot(process: &mut Process, bytes: &[u8]) -> Result<(), String> {
    let records = parse_host_snapshot(bytes)?;

    let old_paths = process
        .registry_host_paths
        .iter()
        .filter(|path| {
            HOST_REGISTRY_ROOTS
                .iter()
                .any(|root| path.as_str() == *root || path.starts_with(&format!("{root}\\")))
        })
        .cloned()
        .collect::<HashSet<_>>();
    process
        .registry_values
        .retain(|(path, _), _| !old_paths.contains(path));
    process
        .registry_host_values
        .retain(|(path, _)| !old_paths.contains(path));
    process
        .registry_paths
        .retain(|path| !old_paths.contains(path));
    process
        .registry_key_metadata
        .retain(|path, _| !old_paths.contains(path));
    process
        .registry_children
        .retain(|path, _| !old_paths.contains(path));
    process
        .registry_host_paths
        .retain(|path| !old_paths.contains(path));

    for record in records {
        let path = normalize_path(&record.path)?;
        process.registry_paths.insert(path.clone());
        process.registry_host_paths.insert(path.clone());
        process.registry_key_metadata.insert(
            path.clone(),
            RegistryKeyMetadata {
                class_name: record.class_name,
                security_descriptor_size: Some(record.security_descriptor_size),
                last_write_filetime: Some(record.last_write_filetime),
            },
        );
        process
            .registry_children
            .insert(path.clone(), record.subkeys);
        for value in record.values {
            let name = value.name.to_lowercase();
            process
                .registry_values
                .insert((path.clone(), name.clone()), (value.value_type, value.data));
            process.registry_host_values.insert((path.clone(), name));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        load_host_snapshot, parse_host_snapshot, HOST_CPU_REGISTRY_ROOT, SIMS_REGISTRY_ROOT,
    };
    use crate::{process::Process, vfs::Vfs};

    const EA_SIMS_REGISTRY_ROOT: &str =
        "hkey_local_machine\\software\\electronic arts\\sims(steam)";

    fn text(output: &mut Vec<u8>, value: &str) {
        output.extend_from_slice(&(value.len() as u32).to_le_bytes());
        output.extend_from_slice(value.as_bytes());
    }

    fn sample_snapshot() -> Vec<u8> {
        let mut bytes = b"AWREG1\0\0".to_vec();
        bytes.extend_from_slice(&6u32.to_le_bytes());
        text(&mut bytes, SIMS_REGISTRY_ROOT);
        text(&mut bytes, "Steam Sims");
        bytes.extend_from_slice(&120u32.to_le_bytes());
        bytes.extend_from_slice(&0x1234u64.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        text(&mut bytes, "The Sims 3");
        bytes.extend_from_slice(&1u32.to_le_bytes());
        text(&mut bytes, "install dir");
        bytes.extend_from_slice(&1u32.to_le_bytes());
        let data = "D:\\The Sims 3\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&data);
        text(&mut bytes, &format!("{SIMS_REGISTRY_ROOT}\\The Sims 3"));
        text(&mut bytes, "Base Game");
        bytes.extend_from_slice(&96u32.to_le_bytes());
        bytes.extend_from_slice(&0x2345u64.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        text(&mut bytes, EA_SIMS_REGISTRY_ROOT);
        text(&mut bytes, "Electronic Arts Sims");
        bytes.extend_from_slice(&120u32.to_le_bytes());
        bytes.extend_from_slice(&0x4567u64.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        text(&mut bytes, "The Sims 3");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        text(&mut bytes, &format!("{EA_SIMS_REGISTRY_ROOT}\\The Sims 3"));
        text(&mut bytes, "Base Game registration");
        bytes.extend_from_slice(&96u32.to_le_bytes());
        bytes.extend_from_slice(&0x5678u64.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        text(&mut bytes, "ergc");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        text(
            &mut bytes,
            &format!("{EA_SIMS_REGISTRY_ROOT}\\The Sims 3\\ergc"),
        );
        text(&mut bytes, "Registration");
        bytes.extend_from_slice(&96u32.to_le_bytes());
        bytes.extend_from_slice(&0x6789u64.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        text(&mut bytes, HOST_CPU_REGISTRY_ROOT);
        text(&mut bytes, "");
        bytes.extend_from_slice(&240u32.to_le_bytes());
        bytes.extend_from_slice(&0x3456u64.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes
    }

    #[test]
    fn parses_and_installs_native_registry_data() {
        let snapshot = sample_snapshot();
        let records = parse_host_snapshot(&snapshot).unwrap();
        assert_eq!(records.len(), 6);
        let mut process = Process::new(Vfs::new());
        load_host_snapshot(&mut process, &snapshot).unwrap();
        let value = process
            .registry_values
            .get(&(SIMS_REGISTRY_ROOT.to_owned(), "install dir".to_owned()))
            .unwrap();
        assert_eq!(value.0, 1);
        assert_eq!(
            String::from_utf16_lossy(
                &value
                    .1
                    .chunks_exact(2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .collect::<Vec<_>>()
            ),
            "D:\\The Sims 3\0"
        );
        assert_eq!(
            process.registry_children[SIMS_REGISTRY_ROOT],
            ["The Sims 3"]
        );
        assert_eq!(
            process.registry_key_metadata[SIMS_REGISTRY_ROOT].last_write_filetime,
            Some(0x1234)
        );
        assert!(process.registry_paths.contains(HOST_CPU_REGISTRY_ROOT));
        assert!(process
            .registry_paths
            .contains(&format!("{EA_SIMS_REGISTRY_ROOT}\\the sims 3\\ergc")));
    }

    #[test]
    fn malformed_or_out_of_scope_snapshots_fail_without_installing_partial_data() {
        let mut process = Process::new(Vfs::new());
        let mut bad = sample_snapshot();
        bad[0] = b'X';
        assert!(load_host_snapshot(&mut process, &bad).is_err());
        assert!(process.registry_paths.is_empty());

        let mut bad = sample_snapshot();
        let root_start = 12;
        bad[root_start..root_start + SIMS_REGISTRY_ROOT.len()]
            .copy_from_slice(b"hkey_local_machine\\software\\sims(other)");
        assert!(parse_host_snapshot(&bad).is_err());
    }
}
