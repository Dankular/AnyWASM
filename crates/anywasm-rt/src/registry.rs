//! Read-only host registry snapshots and the durable virtual HKCU profile overlay.

use crate::process::{Process, RegistryKeyMetadata};
use std::collections::{HashMap, HashSet};

const MAGIC_V1: &[u8; 8] = b"AWREG1\0\0";
const MAGIC_V2: &[u8; 8] = b"AWREG2\0\0";
const USER_PROFILE_MAGIC: &[u8; 8] = b"AWUPR1\0\0";
const MAX_KEYS: usize = 4096;
const MAX_ITEMS: usize = 65_536;
const MAX_TEXT: usize = 1 << 20;
const MAX_VALUE_BYTES: usize = 16 << 20;
const SIMS_REGISTRY_ROOT: &str = "hkey_local_machine\\software\\sims(steam)";
const EA_SIMS_REGISTRY_ROOT: &str = "hkey_local_machine\\software\\electronic arts\\sims(steam)";
const HOST_CPU_REGISTRY_ROOT: &str =
    "hkey_local_machine\\hardware\\description\\system\\centralprocessor\\0";
const HOST_SHELL_FOLDERS_REGISTRY_ROOT: &str =
    "hkey_current_user\\software\\microsoft\\windows\\currentversion\\explorer\\shell folders";
/// Exact 32-bit-view lookup added from the San Andreas runtime trace. This is negative evidence,
/// not an allowlist for arbitrary host registry reads.
pub const HOST_QUICKTIME_REGISTRY_PATH_32: &str =
    "hkey_local_machine\\software\\apple computer, inc.\\quicktime";
/// Exact 32-bit-view host path requested by the San Andreas runtime. The
/// snapshot may contain this key's captured tree or evidence that the key is
/// absent; no sibling Apple registry paths are in scope.
pub const HOST_USER_QUICKTIME_REGISTRY_PATH_32: &str =
    "hkey_current_user\\software\\apple computer, inc.\\quicktime";
const HOST_REGISTRY_ROOTS: [&str; 5] = [
    SIMS_REGISTRY_ROOT,
    EA_SIMS_REGISTRY_ROOT,
    HOST_CPU_REGISTRY_ROOT,
    HOST_SHELL_FOLDERS_REGISTRY_ROOT,
    HOST_USER_QUICKTIME_REGISTRY_PATH_32,
];

fn is_allowed_host_registry_path(path: &str) -> bool {
    path == HOST_QUICKTIME_REGISTRY_PATH_32
        || HOST_REGISTRY_ROOTS.iter().any(|root| {
            path == *root
                || (*root != HOST_SHELL_FOLDERS_REGISTRY_ROOT
                    && path.starts_with(&format!("{root}\\")))
        })
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostRegistrySnapshot {
    pub keys: Vec<RegistryKeyRecord>,
    /// Exact paths verified absent in the host's 32-bit registry view.
    pub missing_paths_32: HashSet<String>,
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

fn is_user_profile_path(path: &str) -> bool {
    path == "hkey_current_user" || path.starts_with("hkey_current_user\\")
}

fn is_host_snapshot_path(path: &str) -> bool {
    HOST_REGISTRY_ROOTS.iter().any(|root| {
        path == *root
            || (*root != HOST_SHELL_FOLDERS_REGISTRY_ROOT && path.starts_with(&format!("{root}\\")))
            || (*root == HOST_SHELL_FOLDERS_REGISTRY_ROOT && path.starts_with(&format!("{root}\\")))
    })
}

fn push_text(bytes: &mut Vec<u8>, text: &str) -> Result<(), String> {
    let length = u32::try_from(text.len())
        .map_err(|_| "user profile text exceeds the format limit".to_owned())?;
    if text.len() > MAX_TEXT || text.contains('\0') {
        return Err("user profile contains an invalid or oversized text field".to_owned());
    }
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(text.as_bytes());
    Ok(())
}

/// Installs a validated, writable per-origin HKCU snapshot. The records are kept separate from
/// host registry keys, which remain read-only and are never written back to browser storage.
pub fn load_user_profile_snapshot(process: &mut Process, bytes: &[u8]) -> Result<(), String> {
    let mut reader = Reader { bytes, offset: 0 };
    if reader.take(USER_PROFILE_MAGIC.len())? != USER_PROFILE_MAGIC {
        return Err("user profile snapshot has an invalid signature".to_owned());
    }
    let key_count = reader.u32()? as usize;
    let value_count = reader.u32()? as usize;
    if key_count > MAX_KEYS || value_count > MAX_ITEMS {
        return Err("user profile snapshot has too many registry values".to_owned());
    }
    let mut keys = HashSet::with_capacity(key_count);
    let mut key_paths = Vec::with_capacity(key_count);
    for _ in 0..key_count {
        let path = normalize_path(&reader.text()?)?;
        if path == "hkey_current_user"
            || !is_user_profile_path(&path)
            || is_host_snapshot_path(&path)
            || !keys.insert(path.clone())
        {
            return Err(format!(
                "user profile snapshot contains an invalid or duplicate key: {path}"
            ));
        }
        key_paths.push(path);
    }
    for path in &key_paths {
        let parent = path
            .rsplit_once('\\')
            .map(|(parent, _)| parent)
            .unwrap_or("");
        if parent != "hkey_current_user" && !keys.contains(parent) {
            return Err(format!("user profile snapshot omits the parent of {path}"));
        }
    }
    let mut values = Vec::with_capacity(value_count);
    let mut seen = HashSet::with_capacity(value_count);
    for _ in 0..value_count {
        let path = normalize_path(&reader.text()?)?;
        if !is_user_profile_path(&path)
            || is_host_snapshot_path(&path)
            || (path != "hkey_current_user" && !keys.contains(&path))
        {
            return Err(format!(
                "user profile snapshot contains a non-profile key: {path}"
            ));
        }
        let name = reader.text()?.to_lowercase();
        let key = (path.clone(), name.clone());
        if !seen.insert(key) {
            return Err(format!(
                "user profile snapshot repeats value {name:?} under {path}"
            ));
        }
        let value_type = reader.u32()?;
        let wide_data = reader.u32()?;
        if wide_data > 1 || (wide_data != 0 && !matches!(value_type, 1 | 2 | 7)) {
            return Err("user profile snapshot has an invalid registry string encoding".to_owned());
        }
        let data_length = reader.u32()? as usize;
        if data_length > MAX_VALUE_BYTES {
            return Err("user profile registry value exceeds the data limit".to_owned());
        }
        let data = reader.take(data_length)?.to_vec();
        if wide_data != 0 && data.len() % 2 != 0 {
            return Err("user profile contains an odd-length UTF-16 registry string".to_owned());
        }
        values.push((path, name, value_type, data, wide_data != 0));
    }
    if reader.offset != bytes.len() {
        return Err("user profile snapshot has trailing bytes".to_owned());
    }

    // Replace only the writable virtual HKCU overlay. Host snapshot records stay intact.
    process.registry_values.retain(|(path, _), _| {
        !is_user_profile_path(path) || process.registry_host_paths.contains(path)
    });
    process.registry_host_values.retain(|(path, _)| {
        !is_user_profile_path(path) || process.registry_host_paths.contains(path)
    });
    process.registry_paths.retain(|path| {
        !is_user_profile_path(path)
            || process.registry_host_paths.contains(path)
            || path == "hkey_current_user"
    });
    process.registry_children.retain(|path, _| {
        !is_user_profile_path(path)
            || process.registry_host_paths.contains(path)
            || path == "hkey_current_user"
    });
    process.registry_key_metadata.retain(|path, _| {
        !is_user_profile_path(path)
            || process.registry_host_paths.contains(path)
            || path == "hkey_current_user"
    });
    process
        .registry_volatile_paths
        .retain(|path| !is_user_profile_path(path));

    let root = String::from("hkey_current_user");
    process.registry_paths.insert(root.clone());
    process.registry_children.entry(root.clone()).or_default();
    process
        .registry_key_metadata
        .entry(root.clone())
        .or_default();
    for path in key_paths {
        let mut parent = root.clone();
        for component in path
            .strip_prefix("hkey_current_user\\")
            .unwrap()
            .split('\\')
        {
            let child_path = format!("{parent}\\{component}");
            let children = process.registry_children.entry(parent.clone()).or_default();
            if !children
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(component))
            {
                children.push(component.to_owned());
            }
            process.registry_paths.insert(child_path.clone());
            process
                .registry_children
                .entry(child_path.clone())
                .or_default();
            process
                .registry_key_metadata
                .entry(child_path.clone())
                .or_default();
            parent = child_path;
        }
    }
    for (path, name, value_type, data, wide_data) in values {
        let value_key = (path.clone(), name);
        process
            .registry_values
            .insert(value_key.clone(), (value_type, data));
        if wide_data {
            process.registry_host_values.insert(value_key);
        } else {
            process.registry_host_values.remove(&value_key);
        }
    }
    if let Some((1 | 2, data)) = process.registry_values.get(&(
        "hkey_current_user\\control panel\\desktop".to_owned(),
        "screensaveactive".to_owned(),
    )) {
        let value = if data.len() >= 4 && data.len() % 2 == 0 {
            String::from_utf16_lossy(
                &data
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .take_while(|unit| *unit != 0)
                    .collect::<Vec<_>>(),
            )
        } else {
            String::from_utf8_lossy(data.split(|byte| *byte == 0).next().unwrap_or_default())
                .into_owned()
        };
        match value.trim() {
            "1" | "true" => process.system_parameters.screen_saver_active = true,
            "0" | "false" => process.system_parameters.screen_saver_active = false,
            _ => {}
        }
    }
    let sticky_path = "hkey_current_user\\control panel\\accessibility\\stickykeys";
    let sticky_name = "flags";
    if let Some((1 | 2, data)) = process
        .registry_values
        .get(&(sticky_path.to_owned(), sticky_name.to_owned()))
    {
        let key = (sticky_path.to_owned(), sticky_name.to_owned());
        let value = if process.registry_host_values.contains(&key) {
            String::from_utf16_lossy(
                &data
                    .chunks_exact(2)
                    .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                    .take_while(|unit| *unit != 0)
                    .collect::<Vec<_>>(),
            )
        } else {
            String::from_utf8_lossy(data.split(|byte| *byte == 0).next().unwrap_or_default())
                .into_owned()
        };
        let parsed = value
            .trim()
            .strip_prefix("0x")
            .or_else(|| value.trim().strip_prefix("0X"))
            .and_then(|digits| u32::from_str_radix(digits, 16).ok())
            .or_else(|| value.trim().parse::<u32>().ok());
        if let Some(flags) = parsed {
            process.system_parameters.sticky_keys_flags = Some(flags);
        }
    }
    process.user_profile_registry_generation = 0;
    Ok(())
}

/// Serializes writable virtual HKCU values to the bounded profile storage format.
pub fn serialize_user_profile_snapshot(process: &Process) -> Result<Vec<u8>, String> {
    let mut entries = process
        .registry_values
        .iter()
        .filter(|((path, _), _)| {
            is_user_profile_path(path)
                && !process.registry_host_paths.contains(path)
                && !is_host_snapshot_path(path)
                && !process.is_registry_volatile_path(path)
        })
        .map(|((path, name), (value_type, data))| {
            (
                path.as_str(),
                name.as_str(),
                *value_type,
                data.as_slice(),
                process
                    .registry_host_values
                    .contains(&(path.clone(), name.clone())),
            )
        })
        .collect::<Vec<_>>();
    entries.sort_unstable_by(|left, right| (left.0, left.1).cmp(&(right.0, right.1)));
    if entries.len() > MAX_ITEMS {
        return Err("user profile has too many registry values to serialize".to_owned());
    }
    let mut keys = process
        .registry_paths
        .iter()
        .filter(|path| {
            is_user_profile_path(path)
                && path.as_str() != "hkey_current_user"
                && !process.registry_host_paths.contains(*path)
                && !is_host_snapshot_path(path)
                && !process.is_registry_volatile_path(path)
        })
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    if keys.len() > MAX_KEYS {
        return Err("user profile has too many registry keys to serialize".to_owned());
    }
    let mut bytes = Vec::with_capacity(16);
    bytes.extend_from_slice(USER_PROFILE_MAGIC);
    bytes.extend_from_slice(&(keys.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for path in keys {
        push_text(&mut bytes, path)?;
    }
    for (path, name, value_type, data, wide_data) in entries {
        if data.len() > MAX_VALUE_BYTES {
            return Err("user profile registry value exceeds the data limit".to_owned());
        }
        push_text(&mut bytes, path)?;
        push_text(&mut bytes, name)?;
        bytes.extend_from_slice(&value_type.to_le_bytes());
        bytes.extend_from_slice(&u32::from(wide_data).to_le_bytes());
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
        if bytes.len() > 32 * 1024 * 1024 {
            return Err("user profile snapshot exceeds 32 MiB".to_owned());
        }
    }
    Ok(bytes)
}

pub fn parse_host_snapshot(bytes: &[u8]) -> Result<HostRegistrySnapshot, String> {
    let mut reader = Reader { bytes, offset: 0 };
    let magic = reader.take(MAGIC_V1.len())?;
    let has_negative_paths = if magic == MAGIC_V2 {
        true
    } else if magic == MAGIC_V1 {
        false
    } else {
        return Err("registry snapshot has an invalid signature".to_owned());
    };
    let key_count = reader.u32()? as usize;
    if key_count == 0 || key_count > MAX_KEYS {
        return Err("registry snapshot key count is outside the accepted range".to_owned());
    }
    let missing_count = if has_negative_paths {
        let count = reader.u32()? as usize;
        if count > MAX_KEYS {
            return Err("registry snapshot has too many confirmed-missing keys".to_owned());
        }
        count
    } else {
        0
    };
    let mut missing_paths_32 = HashSet::with_capacity(missing_count);
    for _ in 0..missing_count {
        let path = normalize_path(&reader.text()?)?;
        if path != HOST_QUICKTIME_REGISTRY_PATH_32 && path != HOST_USER_QUICKTIME_REGISTRY_PATH_32 {
            return Err(format!(
                "registry snapshot has no 32-bit absence evidence for host path: {path}"
            ));
        }
        if !missing_paths_32.insert(path.clone()) {
            return Err(format!(
                "registry snapshot repeats confirmed-missing host path: {path}"
            ));
        }
    }

    let mut records = Vec::with_capacity(key_count);
    let mut by_path = HashMap::with_capacity(key_count);
    for _ in 0..key_count {
        let path = reader.text()?;
        let normalized = normalize_path(&path)?;
        if !is_allowed_host_registry_path(&normalized) {
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
    if missing_paths_32.iter().any(|missing| {
        let descendant_prefix = format!("{missing}\\");
        by_path
            .keys()
            .any(|present| present == missing || present.starts_with(&descendant_prefix))
    }) {
        return Err(
            "registry snapshot marks a present key or descendant as confirmed missing".to_owned(),
        );
    }

    let record_paths = by_path.keys().cloned().collect::<HashSet<_>>();
    let root_present = record_paths.contains(SIMS_REGISTRY_ROOT);
    // Non-Sims launches may receive the shared Shell Folders key and the
    // exact QuickTime key/tree requested by the GTA runtime. Keep the normal
    // Sims snapshot requirements intact and reject every other partial host
    // snapshot.
    let shared_runtime_keys_only = !root_present
        && records.iter().all(|record| {
            let path = record.path.to_lowercase();
            path == HOST_SHELL_FOLDERS_REGISTRY_ROOT
                || path == HOST_USER_QUICKTIME_REGISTRY_PATH_32
                || path.starts_with(&format!("{HOST_USER_QUICKTIME_REGISTRY_PATH_32}\\"))
        });
    if !root_present && !shared_runtime_keys_only {
        return Err("registry snapshot omits the Sims registration root".to_owned());
    }
    if root_present && !record_paths.contains(EA_SIMS_REGISTRY_ROOT) {
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
    Ok(HostRegistrySnapshot {
        keys: records,
        missing_paths_32,
    })
}

/// Installs a validated, read-only snapshot of allowlisted host registry keys.
pub fn load_host_snapshot(process: &mut Process, bytes: &[u8]) -> Result<(), String> {
    let snapshot = parse_host_snapshot(bytes)?;

    let old_paths = process
        .registry_host_paths
        .iter()
        .filter(|path| is_allowed_host_registry_path(path))
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

    process.registry_missing_host_paths_32.clear();

    for record in snapshot.keys {
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
    process
        .registry_missing_host_paths_32
        .extend(snapshot.missing_paths_32);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        is_allowed_host_registry_path, load_host_snapshot, load_user_profile_snapshot,
        parse_host_snapshot, serialize_user_profile_snapshot, HOST_CPU_REGISTRY_ROOT,
        HOST_QUICKTIME_REGISTRY_PATH_32, HOST_SHELL_FOLDERS_REGISTRY_ROOT,
        HOST_USER_QUICKTIME_REGISTRY_PATH_32, SIMS_REGISTRY_ROOT,
    };
    use crate::{process::Process, vfs::Vfs};
    use anywasm_cpu::cpu::{R8, R9, RCX, RDX, RSP};
    use anywasm_cpu::mem::PROT_RW;

    const EA_SIMS_REGISTRY_ROOT: &str =
        "hkey_local_machine\\software\\electronic arts\\sims(steam)";

    fn text(output: &mut Vec<u8>, value: &str) {
        output.extend_from_slice(&(value.len() as u32).to_le_bytes());
        output.extend_from_slice(value.as_bytes());
    }

    #[test]
    fn user_profile_snapshot_roundtrips_values_and_empty_keys_but_omits_volatile_keys() {
        let mut process = Process::new(Vfs::new());
        let desktop = "hkey_current_user\\control panel\\desktop".to_owned();
        let software = "hkey_current_user\\software".to_owned();
        let empty = "hkey_current_user\\software\\anywasm\\empty".to_owned();
        let volatile = "hkey_current_user\\volatile settings".to_owned();
        process.registry_paths.extend([
            "hkey_current_user".to_owned(),
            "hkey_current_user\\control panel".to_owned(),
            desktop.clone(),
            software.clone(),
            "hkey_current_user\\software\\anywasm".to_owned(),
            empty.clone(),
            volatile.clone(),
        ]);
        process.registry_values.insert(
            (desktop.clone(), "screensaveactive".to_owned()),
            (1, b"1\0".to_vec()),
        );
        let wide_name = (desktop.clone(), "widevalue".to_owned());
        process.registry_values.insert(
            wide_name.clone(),
            (
                1,
                "wide\0".encode_utf16().flat_map(u16::to_le_bytes).collect(),
            ),
        );
        process.registry_host_values.insert(wide_name.clone());
        process.registry_volatile_paths.insert(volatile.clone());
        process.registry_values.insert(
            (volatile.clone(), "sessiononly".to_owned()),
            (4, 7u32.to_le_bytes().to_vec()),
        );
        process.system_parameters.screen_saver_active = true;

        let snapshot = serialize_user_profile_snapshot(&process).unwrap();
        let mut restored = Process::new(Vfs::new());
        load_user_profile_snapshot(&mut restored, &snapshot).unwrap();

        assert!(restored.registry_paths.contains(&empty));
        assert!(!restored.registry_paths.contains(&volatile));
        assert_eq!(
            restored
                .registry_values
                .get(&(desktop.clone(), "screensaveactive".to_owned(),)),
            Some(&(1, b"1\0".to_vec()))
        );
        assert_eq!(
            restored.registry_values.get(&wide_name),
            Some(&(
                1,
                "wide\0".encode_utf16().flat_map(u16::to_le_bytes).collect()
            ))
        );
        assert!(restored.registry_host_values.contains(&wide_name));
        assert!(restored.system_parameters.screen_saver_active);
    }

    #[test]
    fn user_profile_snapshot_rejects_host_registry_paths() {
        let mut bytes = b"AWUPR1\0\0".to_vec();
        bytes.extend_from_slice(&1u32.to_le_bytes()); // one key
        bytes.extend_from_slice(&0u32.to_le_bytes()); // no values
        text(&mut bytes, HOST_SHELL_FOLDERS_REGISTRY_ROOT);
        let error = load_user_profile_snapshot(&mut Process::new(Vfs::new()), &bytes).unwrap_err();
        assert!(error.contains("invalid or duplicate key"));
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

    fn sample_snapshot_with_quicktime_absence_32() -> Vec<u8> {
        let v1 = sample_snapshot();
        let mut bytes = b"AWREG2\0\0".to_vec();
        bytes.extend_from_slice(&6u32.to_le_bytes());
        bytes.extend_from_slice(&2u32.to_le_bytes());
        text(&mut bytes, HOST_QUICKTIME_REGISTRY_PATH_32);
        text(&mut bytes, HOST_USER_QUICKTIME_REGISTRY_PATH_32);
        bytes.extend_from_slice(&v1[12..]);
        bytes
    }

    fn snapshot_with_user_quicktime_tree_32(mark_quicktime_root_missing: bool) -> Vec<u8> {
        let mut bytes = b"AWREG2\0\0".to_vec();
        bytes.extend_from_slice(&3u32.to_le_bytes()); // Shell Folders plus QuickTime root/tree.
        bytes.extend_from_slice(
            &(if mark_quicktime_root_missing {
                2u32
            } else {
                1u32
            })
            .to_le_bytes(),
        );
        text(&mut bytes, HOST_QUICKTIME_REGISTRY_PATH_32);
        if mark_quicktime_root_missing {
            text(&mut bytes, HOST_USER_QUICKTIME_REGISTRY_PATH_32);
        }

        text(&mut bytes, HOST_SHELL_FOLDERS_REGISTRY_ROOT);
        text(&mut bytes, "");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes()); // no Shell Folders subkeys
        bytes.extend_from_slice(&0u32.to_le_bytes()); // no Shell Folders values

        text(&mut bytes, HOST_USER_QUICKTIME_REGISTRY_PATH_32);
        text(&mut bytes, "QuickTime");
        bytes.extend_from_slice(&120u32.to_le_bytes());
        bytes.extend_from_slice(&0x1234u64.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        text(&mut bytes, "Preferences");
        bytes.extend_from_slice(&0u32.to_le_bytes()); // no values at the root

        text(
            &mut bytes,
            &format!("{HOST_USER_QUICKTIME_REGISTRY_PATH_32}\\Preferences"),
        );
        text(&mut bytes, "Preferences");
        bytes.extend_from_slice(&96u32.to_le_bytes());
        bytes.extend_from_slice(&0x5678u64.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes()); // no subkeys
        bytes.extend_from_slice(&0u32.to_le_bytes()); // no values
        bytes
    }

    fn snapshot_with_shell_folders() -> Vec<u8> {
        let mut snapshot = b"AWREG1\0\0".to_vec();
        snapshot.extend_from_slice(&1u32.to_le_bytes());
        text(&mut snapshot, HOST_SHELL_FOLDERS_REGISTRY_ROOT);
        text(&mut snapshot, "");
        snapshot.extend_from_slice(&0u32.to_le_bytes());
        snapshot.extend_from_slice(&0u64.to_le_bytes());
        snapshot.extend_from_slice(&0u32.to_le_bytes());
        snapshot.extend_from_slice(&1u32.to_le_bytes());
        text(&mut snapshot, "Personal");
        snapshot.extend_from_slice(&1u32.to_le_bytes()); // REG_SZ
        let data = "C:\\Users\\tester\\Documents\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        snapshot.extend_from_slice(&(data.len() as u32).to_le_bytes());
        snapshot.extend_from_slice(&data);
        snapshot
    }

    #[test]
    fn parses_and_installs_native_registry_data() {
        let snapshot = sample_snapshot();
        let records = parse_host_snapshot(&snapshot).unwrap();
        assert_eq!(records.keys.len(), 6);
        assert!(records.missing_paths_32.is_empty());
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
    fn installs_exact_confirmed_32_bit_view_absence_without_widening_host_reads() {
        let snapshot = sample_snapshot_with_quicktime_absence_32();
        let parsed = parse_host_snapshot(&snapshot).unwrap();
        assert_eq!(parsed.missing_paths_32.len(), 2);
        assert!(parsed
            .missing_paths_32
            .contains(HOST_QUICKTIME_REGISTRY_PATH_32));
        assert!(parsed
            .missing_paths_32
            .contains(HOST_USER_QUICKTIME_REGISTRY_PATH_32));

        let mut process = Process::new(Vfs::new());
        load_host_snapshot(&mut process, &snapshot).unwrap();
        assert!(process
            .registry_missing_host_paths_32
            .contains(HOST_QUICKTIME_REGISTRY_PATH_32));
        assert!(!process
            .registry_paths
            .contains(HOST_QUICKTIME_REGISTRY_PATH_32));
        assert!(!is_allowed_host_registry_path(
            "hkey_local_machine\\software\\apple computer, inc.\\quicktime\\private"
        ));
        assert!(!is_allowed_host_registry_path(
            "hkey_current_user\\software\\apple computer, inc.\\other"
        ));

        let mut malformed = snapshot.clone();
        let bad_path = b"hkey_local_machine\\software\\apple computer, inc.\\unknown";
        let start = 16 + 4;
        malformed[start..start + bad_path.len()].copy_from_slice(bad_path);
        assert!(parse_host_snapshot(&malformed).is_err());
    }

    #[test]
    fn accepts_only_the_exact_current_user_quicktime_tree_in_a_partial_host_snapshot() {
        let snapshot = snapshot_with_user_quicktime_tree_32(false);
        let parsed = parse_host_snapshot(&snapshot).unwrap();
        assert_eq!(parsed.keys.len(), 3);
        assert!(parsed
            .keys
            .iter()
            .any(|record| record.path.to_lowercase() == HOST_USER_QUICKTIME_REGISTRY_PATH_32));
        assert!(parsed.keys.iter().any(|record| {
            record.path.to_lowercase()
                == format!("{HOST_USER_QUICKTIME_REGISTRY_PATH_32}\\preferences")
        }));
        assert!(parsed
            .missing_paths_32
            .contains(HOST_QUICKTIME_REGISTRY_PATH_32));
        assert!(is_allowed_host_registry_path(
            HOST_USER_QUICKTIME_REGISTRY_PATH_32
        ));
        assert!(is_allowed_host_registry_path(&format!(
            "{HOST_USER_QUICKTIME_REGISTRY_PATH_32}\\preferences"
        )));
        assert!(!is_allowed_host_registry_path(
            "hkey_current_user\\software\\apple computer, inc."
        ));
        assert!(!is_allowed_host_registry_path(
            "hkey_current_user\\software\\apple computer, inc.\\unrelated"
        ));

        let mut process = Process::new(Vfs::new());
        load_host_snapshot(&mut process, &snapshot).unwrap();
        assert!(process
            .registry_host_paths
            .contains(HOST_USER_QUICKTIME_REGISTRY_PATH_32));
        assert!(process.registry_host_paths.contains(&format!(
            "{HOST_USER_QUICKTIME_REGISTRY_PATH_32}\\preferences"
        )));

        assert!(
            parse_host_snapshot(&snapshot_with_user_quicktime_tree_32(true))
                .unwrap_err()
                .contains("present key or descendant as confirmed missing")
        );
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

    #[test]
    fn accepts_scoped_shell_folders_and_quicktime_roots_as_partial_host_snapshots() {
        let records = parse_host_snapshot(&snapshot_with_shell_folders()).unwrap();
        assert_eq!(records.keys.len(), 1);
        assert_eq!(records.keys[0].path, HOST_SHELL_FOLDERS_REGISTRY_ROOT);

        let mut process = Process::new(Vfs::new());
        let mut partial_sims = sample_snapshot();
        partial_sims[8..12].copy_from_slice(&1u32.to_le_bytes());
        // Keep the malformed remainder intact: either way this is not the
        // exact one-key exception and must not be accepted as a partial view.
        assert!(load_host_snapshot(&mut process, &partial_sims).is_err());
        assert!(process.registry_paths.is_empty());
    }

    #[test]
    fn host_snapshot_manager_installs_only_the_scoped_current_user_shell_folders_key() {
        let snapshot = snapshot_with_shell_folders();
        let mut process = Process::new(Vfs::new());
        load_host_snapshot(&mut process, &snapshot).unwrap();
        assert!(process
            .registry_paths
            .contains(HOST_SHELL_FOLDERS_REGISTRY_ROOT));
        let (value_type, value_data) = process
            .registry_values
            .get(&(
                HOST_SHELL_FOLDERS_REGISTRY_ROOT.to_owned(),
                "personal".to_owned(),
            ))
            .unwrap();
        assert_eq!(*value_type, 1);
        assert_eq!(
            String::from_utf16_lossy(
                &value_data
                    .chunks_exact(2)
                    .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
                    .collect::<Vec<_>>()
            ),
            "C:\\Users\\tester\\Documents\0"
        );
        assert!(process
            .registry_host_paths
            .contains(HOST_SHELL_FOLDERS_REGISTRY_ROOT));
        assert!(is_allowed_host_registry_path(
            HOST_SHELL_FOLDERS_REGISTRY_ROOT
        ));
        assert!(!is_allowed_host_registry_path(&format!(
            "{HOST_SHELL_FOLDERS_REGISTRY_ROOT}\\private"
        )));
    }

    #[test]
    fn advapi_opens_and_reads_the_exact_host_shell_folders_key() {
        let mut process = Process::new(Vfs::new());
        load_host_snapshot(&mut process, &snapshot_with_shell_folders()).unwrap();

        let args = process.alloc_virtual(0x1000, PROT_RW);
        let subkey = args + 0x100;
        let opened_key = args + 0x200;
        let value_name = args + 0x300;
        let value_data = args + 0x400;
        let value_type = args + 0x800;
        let value_size = args + 0x804;
        process
            .write_cstr(
                subkey,
                "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Shell Folders",
            )
            .unwrap();
        process.write_cstr(value_name, "Personal").unwrap();

        let stack = args + 0xc00;
        process.cpu.gpr[RCX] = 0x8000_0001; // HKEY_CURRENT_USER
        process.cpu.gpr[RDX] = subkey;
        process.cpu.gpr[R8] = 0;
        process.cpu.gpr[R9] = 0x0002_0019; // KEY_READ
        process.cpu.gpr[RSP] = stack;
        process.wr64(stack + 40, opened_key).unwrap();

        let open = crate::hle::lookup("advapi32", "RegOpenKeyExA").unwrap();
        assert_eq!(open(&mut process), Ok(0));
        let key = process.rd64(opened_key).unwrap();
        assert_eq!(
            process.registry_key_paths.get(&key).map(String::as_str),
            Some(HOST_SHELL_FOLDERS_REGISTRY_ROOT)
        );

        process.cpu.gpr[RCX] = key;
        process.cpu.gpr[RDX] = value_name;
        process.cpu.gpr[R8] = 0;
        process.cpu.gpr[R9] = value_type;
        process.wr64(stack + 40, value_data).unwrap();
        process.wr64(stack + 48, value_size).unwrap();
        process.wr32(value_size, 512).unwrap();

        let query = crate::hle::lookup("advapi32", "RegQueryValueExA").unwrap();
        assert_eq!(query(&mut process), Ok(0));
        assert_eq!(process.rd32(value_type).unwrap(), 1); // REG_SZ
        assert_eq!(
            process.read_cstr(value_data).unwrap(),
            "C:\\Users\\tester\\Documents"
        );
    }
}
