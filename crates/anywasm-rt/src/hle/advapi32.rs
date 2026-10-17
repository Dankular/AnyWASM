use crate::process::{
    CryptoHashState, CryptoKeyState, CryptoProviderState, Handle, HleFn, Process, Rc4State,
    VIRTUAL_USER_NAME,
};
use crate::Stop;
use md5::{Digest, Md5};
use std::collections::{HashMap, HashSet};

/// Process-local state for security objects owned by Advapi32.
#[derive(Default)]
pub struct AdvapiState {
    /// Addresses returned by AllocateAndInitializeSid, the only SID storage FreeSid may release.
    pub sid_allocations: HashSet<u64>,
    /// Optional host-provided identity. None means no authoritative SID/account mapping exists.
    pub process_identity: Option<TokenIdentity>,
    /// Token state keyed by guest handles; kernel32's general handle table controls validity.
    pub tokens: HashMap<u64, TokenHandle>,
    /// Current impersonation tokens keyed by guest thread ID.
    pub thread_tokens: HashMap<u32, AccessToken>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenIdentity {
    pub sid: Vec<u8>,
    pub account_name: String,
    pub domain_name: String,
    pub sid_name_use: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenType {
    Primary,
    Impersonation,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessToken {
    pub identity: Option<TokenIdentity>,
    pub token_type: TokenType,
    pub impersonation_level: u32,
    pub allowed_access: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenHandle {
    pub token: AccessToken,
    pub granted_access: u32,
}

#[cfg(windows)]
#[link(name = "bcrypt")]
extern "system" {
    fn BCryptGenRandom(
        algorithm: *mut core::ffi::c_void,
        buffer: *mut u8,
        length: u32,
        flags: u32,
    ) -> i32;
}

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "anywasm")]
extern "C" {
    #[link_name = "random_fill"]
    fn anywasm_random_fill(buffer: *mut u8, length: u32) -> u32;
}

pub fn lookup(name: &str) -> Option<HleFn> {
    match name {
        "GetUserNameA" => Some(get_user_name_a),
        "GetUserNameW" => Some(get_user_name_w),
        "RegOpenKeyA" => Some(reg_open_key_a),
        "RegCreateKeyA" => Some(reg_create_key_a),
        "RegOpenKeyExA" => Some(reg_open_key_ex_a),
        "RegCreateKeyExA" => Some(reg_create_key_ex_a),
        "RegSetValueExA" => Some(reg_set_value_ex_a),
        "RegSetValueExW" => Some(reg_set_value_ex_w),
        "RegQueryValueExA" => Some(reg_query_value_ex_a),
        "RegQueryValueExW" => Some(reg_query_value_ex_w),
        "RegQueryInfoKeyA" => Some(reg_query_info_key_a),
        "RegEnumKeyExA" => Some(reg_enum_key_ex_a),
        "RegCloseKey" => Some(reg_close_key),
        "CryptAcquireContextA" => Some(crypt_acquire_context_a),
        "CryptCreateHash" => Some(crypt_create_hash),
        "CryptHashData" => Some(crypt_hash_data),
        "CryptGetHashParam" => Some(crypt_get_hash_param),
        "CryptDeriveKey" => Some(crypt_derive_key),
        "CryptEncrypt" => Some(crypt_encrypt),
        "CryptDecrypt" => Some(crypt_decrypt),
        "CryptDestroyKey" => Some(crypt_destroy_key),
        "CryptDestroyHash" => Some(crypt_destroy_hash),
        "CryptGenRandom" => Some(crypt_gen_random),
        "CryptReleaseContext" => Some(crypt_release_context),
        "SystemFunction036" => Some(system_function_036),
        "AllocateAndInitializeSid" => Some(allocate_and_initialize_sid),
        "BuildTrusteeWithSidW" => Some(build_trustee_with_sid_w),
        "CopySid" => Some(copy_sid),
        "FreeSid" => Some(free_sid),
        "GetLengthSid" => Some(get_length_sid),
        "OpenProcessToken" => Some(open_process_token),
        "OpenThreadToken" => Some(open_thread_token),
        "DuplicateToken" => Some(duplicate_token),
        "GetTokenInformation" => Some(get_token_information),
        "ImpersonateLoggedOnUser" => Some(impersonate_logged_on_user),
        "RevertToSelf" => Some(revert_to_self),
        "LookupAccountSidW" => Some(lookup_account_sid_w),
        _ => None,
    }
}

const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
const SID_REVISION: u8 = 1;
const SID_MAX_SUB_AUTHORITIES: u8 = 15;

fn sid_length(process: &Process, sid: u64) -> Result<Option<usize>, Stop> {
    if sid == 0 {
        return Ok(None);
    }
    let revision = process.rd8(sid)?;
    let count = process.rd8(sid + 1)?;
    if revision != SID_REVISION || count > SID_MAX_SUB_AUTHORITIES {
        return Ok(None);
    }
    Ok(Some(8 + count as usize * 4))
}

fn get_length_sid(process: &mut Process) -> Result<u64, Stop> {
    Ok(sid_length(process, process.arg(0))?.unwrap_or(0) as u64)
}

const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_NOT_SUPPORTED: u32 = 50;
const ERROR_NO_TOKEN: u32 = 1008;
const ERROR_CANT_OPEN_ANONYMOUS: u32 = 1347;
const ERROR_NONE_MAPPED: u32 = 1332;
const TOKEN_ASSIGN_PRIMARY: u32 = 0x0001;
const TOKEN_DUPLICATE: u32 = 0x0002;
const TOKEN_IMPERSONATE: u32 = 0x0004;
const TOKEN_QUERY: u32 = 0x0008;
const TOKEN_QUERY_SOURCE: u32 = 0x0010;
const TOKEN_ADJUST_PRIVILEGES: u32 = 0x0020;
const TOKEN_ADJUST_GROUPS: u32 = 0x0040;
const TOKEN_ADJUST_DEFAULT: u32 = 0x0080;
const TOKEN_ADJUST_SESSIONID: u32 = 0x0100;
const TOKEN_MAXIMUM_ALLOWED: u32 = 0x0200_0000;
const TOKEN_SUPPORTED_ACCESS: u32 = TOKEN_DUPLICATE | TOKEN_IMPERSONATE | TOKEN_QUERY;
const TOKEN_USER_CLASS: u32 = 1;
const SECURITY_ANONYMOUS: u32 = 0;
const SECURITY_IDENTIFICATION: u32 = 1;
const SECURITY_IMPERSONATION: u32 = 2;
const SECURITY_DELEGATION: u32 = 3;
const SID_TYPE_USER: u32 = 1;
const CURRENT_PROCESS_X86: u64 = u32::MAX as u64;
const CURRENT_THREAD_X86: u64 = (u32::MAX - 1) as u64;
const CURRENT_THREAD_X64: u64 = u64::MAX - 1;

fn valid_sid_bytes(sid: &[u8]) -> bool {
    sid.len() >= 8
        && sid[0] == SID_REVISION
        && sid[1] <= SID_MAX_SUB_AUTHORITIES
        && sid.len() == 8 + sid[1] as usize * 4
}

fn validate_identity(identity: &TokenIdentity) -> bool {
    valid_sid_bytes(&identity.sid)
        && !identity.account_name.is_empty()
        && !identity.domain_name.is_empty()
        && identity.sid_name_use == SID_TYPE_USER
}

fn requested_token_access(access: u32) -> Result<u32, u32> {
    if access & !TOKEN_SUPPORTED_ACCESS != 0
        || access & TOKEN_MAXIMUM_ALLOWED != 0
        || access
            & (TOKEN_ASSIGN_PRIMARY
                | TOKEN_QUERY_SOURCE
                | TOKEN_ADJUST_PRIVILEGES
                | TOKEN_ADJUST_GROUPS
                | TOKEN_ADJUST_DEFAULT
                | TOKEN_ADJUST_SESSIONID)
            != 0
    {
        return Err(ERROR_NOT_SUPPORTED);
    }
    Ok(access)
}

fn token_for_handle(process: &Process, handle: u64) -> Option<&TokenHandle> {
    if !matches!(process.handles.get(&handle), Some(Handle::Other)) {
        return None;
    }
    process.extra.advapi32.tokens.get(&handle)
}

fn create_token_handle(process: &mut Process, token: AccessToken, granted_access: u32) -> u64 {
    let handle = process.new_handle(Handle::Other);
    process.extra.advapi32.tokens.insert(
        handle,
        TokenHandle {
            token,
            granted_access,
        },
    );
    handle
}

fn initialize_handle_output(process: &mut Process, output: u64) -> Result<bool, Stop> {
    if output == 0 {
        process.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(false);
    }
    process.wrptr(output, 0)?;
    Ok(true)
}

fn open_process_token(process: &mut Process) -> Result<u64, Stop> {
    let (process_handle, desired_access, output) =
        (process.arg(0), process.arg(1) as u32, process.arg(2));
    if !initialize_handle_output(process, output)? {
        return Ok(0);
    }
    let is_current = matches!(process_handle, CURRENT_PROCESS_X86 | u64::MAX);
    if !is_current {
        if matches!(
            process.handles.get(&process_handle),
            Some(Handle::Process { .. })
        ) {
            process.set_last_error(ERROR_NOT_SUPPORTED);
        } else {
            process.set_last_error(ERROR_INVALID_HANDLE);
        }
        return Ok(0);
    }
    let requested_access = match requested_token_access(desired_access) {
        Ok(access) => access,
        Err(error) => {
            process.set_last_error(error);
            return Ok(0);
        }
    };
    let identity = process.extra.advapi32.process_identity.clone();
    if identity
        .as_ref()
        .is_some_and(|value| !validate_identity(value))
    {
        process.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let token = AccessToken {
        identity,
        token_type: TokenType::Primary,
        impersonation_level: SECURITY_DELEGATION,
        allowed_access: TOKEN_SUPPORTED_ACCESS,
    };
    let handle = create_token_handle(process, token, requested_access);
    process.wrptr(output, handle)?;
    Ok(1)
}

fn open_thread_token(process: &mut Process) -> Result<u64, Stop> {
    let (thread_handle, desired_access, open_as_self, output) = (
        process.arg(0),
        process.arg(1) as u32,
        process.arg(2) != 0,
        process.arg(3),
    );
    if !initialize_handle_output(process, output)? {
        return Ok(0);
    }
    let thread_id = if matches!(thread_handle, CURRENT_THREAD_X86 | CURRENT_THREAD_X64) {
        process.current_thread_id
    } else {
        match process.handles.get(&thread_handle) {
            Some(Handle::Thread { id, .. }) => *id,
            _ => {
                process.set_last_error(ERROR_INVALID_HANDLE);
                return Ok(0);
            }
        }
    };
    let Some(token) = process
        .extra
        .advapi32
        .thread_tokens
        .get(&thread_id)
        .cloned()
    else {
        process.set_last_error(ERROR_NO_TOKEN);
        return Ok(0);
    };
    if token.impersonation_level == SECURITY_ANONYMOUS {
        process.set_last_error(ERROR_CANT_OPEN_ANONYMOUS);
        return Ok(0);
    }
    if !open_as_self && token.impersonation_level == SECURITY_IDENTIFICATION {
        process.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    }
    let requested_access = match requested_token_access(desired_access) {
        Ok(access) => access,
        Err(error) => {
            process.set_last_error(error);
            return Ok(0);
        }
    };
    if requested_access & !token.allowed_access != 0 {
        process.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    }
    let handle = create_token_handle(process, token, requested_access);
    process.wrptr(output, handle)?;
    Ok(1)
}

fn duplicate_token(process: &mut Process) -> Result<u64, Stop> {
    let (source_handle, impersonation_level, output) =
        (process.arg(0), process.arg(1) as u32, process.arg(2));
    if !initialize_handle_output(process, output)? {
        return Ok(0);
    }
    let Some(source) = token_for_handle(process, source_handle).cloned() else {
        process.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    };
    if source.granted_access & TOKEN_DUPLICATE == 0 {
        process.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    }
    if impersonation_level > SECURITY_DELEGATION {
        process.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if source.token.token_type == TokenType::Impersonation
        && impersonation_level > source.token.impersonation_level
    {
        process.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    }
    let token = AccessToken {
        identity: source.token.identity,
        token_type: TokenType::Impersonation,
        impersonation_level,
        allowed_access: TOKEN_QUERY | TOKEN_IMPERSONATE,
    };
    let granted_access = TOKEN_QUERY | TOKEN_IMPERSONATE;
    let handle = create_token_handle(process, token, granted_access);
    process.wrptr(output, handle)?;
    Ok(1)
}

fn get_token_information(process: &mut Process) -> Result<u64, Stop> {
    let (handle, info_class, info, info_len, return_len) = (
        process.arg(0),
        process.arg(1) as u32,
        process.arg(2),
        process.arg(3) as u32 as usize,
        process.arg(4),
    );
    if return_len == 0 || (info == 0 && info_len != 0) {
        process.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    process.wr32(return_len, 0)?;
    let Some(token_handle) = token_for_handle(process, handle).cloned() else {
        process.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    };
    if token_handle.granted_access & TOKEN_QUERY == 0 {
        process.set_last_error(ERROR_ACCESS_DENIED);
        return Ok(0);
    }
    if info_class != TOKEN_USER_CLASS {
        process.set_last_error(ERROR_NOT_SUPPORTED);
        return Ok(0);
    }
    let Some(identity) = token_handle.token.identity else {
        // No host/runtime identity provider has supplied a SID. Do not synthesize one from
        // USERNAME=AnyWASM; callers must observe that TokenUser is unavailable.
        process.set_last_error(ERROR_NOT_SUPPORTED);
        return Ok(0);
    };
    if !validate_identity(&identity) {
        process.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }

    let pointer_size = if process.cpu.mode == anywasm_cpu::cpu::CpuMode::X86_32 {
        4usize
    } else {
        8usize
    };
    let token_user_header_size: usize = if pointer_size == 4 { 8 } else { 16 };
    let required = token_user_header_size
        .checked_add(identity.sid.len())
        .ok_or_else(|| Stop::Crash("TOKEN_USER size overflow".into()))?;
    let required_u32 = u32::try_from(required)
        .map_err(|_| Stop::Crash("TOKEN_USER exceeds DWORD length".into()))?;
    process.wr32(return_len, required_u32)?;
    if info == 0 || info_len < required {
        process.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(0);
    }

    process.write_bytes(info, &vec![0; token_user_header_size])?;
    process.wrptr(info, info + token_user_header_size as u64)?;
    process.write_bytes(info + pointer_size as u64, &0u32.to_le_bytes())?;
    process.write_bytes(info + token_user_header_size as u64, &identity.sid)?;
    Ok(1)
}

fn impersonate_logged_on_user(process: &mut Process) -> Result<u64, Stop> {
    let handle = process.arg(0);
    let Some(token_handle) = token_for_handle(process, handle).cloned() else {
        process.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(0);
    };
    let token = &token_handle.token;
    let impersonation_level = match token.token_type {
        TokenType::Primary => {
            if token_handle.granted_access & (TOKEN_QUERY | TOKEN_DUPLICATE)
                != TOKEN_QUERY | TOKEN_DUPLICATE
            {
                process.set_last_error(ERROR_ACCESS_DENIED);
                return Ok(0);
            }
            SECURITY_IMPERSONATION
        }
        TokenType::Impersonation => {
            if token_handle.granted_access & (TOKEN_QUERY | TOKEN_IMPERSONATE)
                != TOKEN_QUERY | TOKEN_IMPERSONATE
            {
                process.set_last_error(ERROR_ACCESS_DENIED);
                return Ok(0);
            }
            if token.impersonation_level < SECURITY_IMPERSONATION {
                process.set_last_error(ERROR_ACCESS_DENIED);
                return Ok(0);
            }
            token.impersonation_level
        }
    };
    if token.identity.is_none() {
        process.set_last_error(ERROR_NOT_SUPPORTED);
        return Ok(0);
    }
    let impersonation = AccessToken {
        identity: token.identity.clone(),
        token_type: TokenType::Impersonation,
        impersonation_level,
        allowed_access: TOKEN_QUERY | TOKEN_IMPERSONATE,
    };
    process
        .extra
        .advapi32
        .thread_tokens
        .insert(process.current_thread_id, impersonation);
    Ok(1)
}

fn revert_to_self(process: &mut Process) -> Result<u64, Stop> {
    process
        .extra
        .advapi32
        .thread_tokens
        .remove(&process.current_thread_id);
    Ok(1)
}

fn lookup_account_sid_w(process: &mut Process) -> Result<u64, Stop> {
    let (system_name, sid, name, name_len, domain, domain_len, sid_use) = (
        process.arg(0),
        process.arg(1),
        process.arg(2),
        process.arg(3),
        process.arg(4),
        process.arg(5),
        process.arg(6),
    );
    if sid == 0 || name_len == 0 || domain_len == 0 || sid_use == 0 {
        process.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let Some(sid_size) = sid_length(process, sid)? else {
        process.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    };
    let sid_bytes = process.read_bytes(sid, sid_size)?;

    if system_name != 0 {
        let requested_system = process.read_wstr(system_name)?;
        let environment = process.initial_env.as_ref().unwrap_or(&process.env);
        let actual_system = environment
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("COMPUTERNAME"))
            .map(|(_, value)| value.as_str())
            .unwrap_or("ANYWASM");
        if !requested_system.eq_ignore_ascii_case(actual_system) {
            process.set_last_error(ERROR_NONE_MAPPED);
            return Ok(0);
        }
    }

    let identity = process
        .extra
        .advapi32
        .process_identity
        .iter()
        .chain(
            process
                .extra
                .advapi32
                .tokens
                .iter()
                .filter(|(handle, _)| matches!(process.handles.get(handle), Some(Handle::Other)))
                .filter_map(|(_, token)| token.token.identity.as_ref()),
        )
        .chain(
            process
                .extra
                .advapi32
                .thread_tokens
                .values()
                .filter_map(|token| token.identity.as_ref()),
        )
        .find(|candidate| candidate.sid == sid_bytes)
        .cloned();
    let Some(identity) = identity else {
        process.set_last_error(ERROR_NONE_MAPPED);
        return Ok(0);
    };
    if !validate_identity(&identity) {
        process.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }

    let required_name = u32::try_from(identity.account_name.encode_utf16().count() + 1)
        .map_err(|_| Stop::Crash("account name exceeds DWORD length".into()))?;
    let required_domain = u32::try_from(identity.domain_name.encode_utf16().count() + 1)
        .map_err(|_| Stop::Crash("account domain exceeds DWORD length".into()))?;
    let name_capacity = process.rd32(name_len)?;
    let domain_capacity = process.rd32(domain_len)?;
    process.wr32(name_len, required_name)?;
    process.wr32(domain_len, required_domain)?;
    process.wr32(sid_use, identity.sid_name_use)?;
    if name == 0
        || domain == 0
        || name_capacity < required_name
        || domain_capacity < required_domain
    {
        process.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(0);
    }
    process.write_wstr(name, &identity.account_name)?;
    process.write_wstr(domain, &identity.domain_name)?;
    Ok(1)
}

fn copy_sid(process: &mut Process) -> Result<u64, Stop> {
    let (destination_length, destination, source) = (
        process.arg(0) as u32 as usize,
        process.arg(1),
        process.arg(2),
    );
    if destination == 0 || source == 0 {
        process.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let Some(length) = sid_length(process, source)? else {
        process.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    };
    if destination_length < length {
        process.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(0);
    }
    let bytes = process.read_bytes(source, length)?;
    process.write_bytes(destination, &bytes)?;
    Ok(1)
}

fn allocate_and_initialize_sid(process: &mut Process) -> Result<u64, Stop> {
    let authority_ptr = process.arg(0);
    let subauthority_count = process.arg(1) as u8;
    let output = process.arg(10);
    if authority_ptr == 0 || output == 0 || subauthority_count > 8 {
        process.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let authority = process.read_bytes(authority_ptr, 6)?;
    let length = 8 + subauthority_count as usize * 4;
    let mut sid = Vec::with_capacity(length);
    sid.push(SID_REVISION);
    sid.push(subauthority_count);
    sid.extend_from_slice(&authority);
    for index in 0..subauthority_count as usize {
        sid.extend_from_slice(&(process.arg(index + 2) as u32).to_le_bytes());
    }
    let mut mapped = Vec::new();
    let address = process.heap.alloc(length as u64, &mut |base, size| {
        mapped.push((base, size));
        true
    });
    let Some(address) = address else {
        process.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
        return Ok(0);
    };
    for (base, size) in mapped {
        if !process.mem.map(base, size, anywasm_cpu::mem::PROT_RW) {
            process.heap.free(address);
            process.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
            return Ok(0);
        }
    }
    if let Err(error) = process.write_bytes(address, &sid) {
        process.heap.free(address);
        return Err(error);
    }
    if let Err(error) = process.wrptr(output, address) {
        process.heap.free(address);
        return Err(error);
    }
    process.extra.advapi32.sid_allocations.insert(address);
    Ok(1)
}

fn free_sid(process: &mut Process) -> Result<u64, Stop> {
    let sid = process.arg(0);
    if process.extra.advapi32.sid_allocations.remove(&sid) && process.heap.free(sid).is_some() {
        Ok(0)
    } else {
        Ok(sid)
    }
}

fn build_trustee_with_sid_w(process: &mut Process) -> Result<u64, Stop> {
    let (trustee, sid) = (process.arg(0), process.arg(1));
    if trustee == 0 {
        return Err(Stop::Crash(
            "BuildTrusteeWithSidW received a null TRUSTEE_W".into(),
        ));
    }
    let pointer_size = if process.cpu.mode == anywasm_cpu::cpu::CpuMode::X86_32 {
        4u64
    } else {
        8u64
    };
    let enum_offset = pointer_size;
    let name_offset = if pointer_size == 4 { 16 } else { 24 };
    process.wrptr(trustee, 0)?; // pMultipleTrustee
    process.wr32(trustee + enum_offset, 0)?; // NO_MULTIPLE_TRUSTEE
    process.wr32(trustee + enum_offset + 4, 0)?; // TRUSTEE_IS_SID
    process.wr32(trustee + enum_offset + 8, 0)?; // TRUSTEE_IS_UNKNOWN
    process.wrptr(trustee + name_offset, sid)?;
    Ok(0)
}

fn crypt_acquire_context_a(process: &mut Process) -> Result<u64, Stop> {
    const ERROR_INVALID_PARAMETER: u32 = 87;
    const NTE_BAD_FLAGS: u32 = 0x8009_0009;
    const NTE_BAD_PROVIDER: u32 = 0x8009_0013;
    const NTE_BAD_PROV_TYPE: u32 = 0x8009_0014;
    const PROV_RSA_FULL: u32 = 1;
    const CRYPT_VERIFYCONTEXT: u32 = 0xf000_0000;
    const CRYPT_SILENT: u32 = 0x0000_0040;
    const DEFAULT_PROVIDER: &str = "Microsoft Base Cryptographic Provider v1.0";

    let (provider_out, container_ptr, provider_ptr, provider_type, flags) = (
        process.arg(0),
        process.arg(1),
        process.arg(2),
        process.arg(3) as u32,
        process.arg(4) as u32,
    );
    if provider_out == 0 {
        process.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if provider_type != PROV_RSA_FULL {
        process.set_last_error(NTE_BAD_PROV_TYPE);
        return Ok(0);
    }
    let provider_name = if provider_ptr == 0 {
        DEFAULT_PROVIDER.to_owned()
    } else {
        process.read_cstr(provider_ptr)?
    };
    if !provider_name.eq_ignore_ascii_case(DEFAULT_PROVIDER) {
        process.set_last_error(NTE_BAD_PROVIDER);
        return Ok(0);
    }

    let allowed_flags = CRYPT_VERIFYCONTEXT | CRYPT_SILENT;
    if flags & CRYPT_VERIFYCONTEXT != CRYPT_VERIFYCONTEXT || flags & !allowed_flags != 0 {
        if flags & CRYPT_VERIFYCONTEXT != CRYPT_VERIFYCONTEXT {
            return Err(Stop::Unimplemented {
                dll: "advapi32".to_owned(),
                name: "CryptAcquireContextA persistent key-container semantics are not translated"
                    .to_owned(),
            });
        }
        process.set_last_error(NTE_BAD_FLAGS);
        return Ok(0);
    }
    if container_ptr != 0 {
        process.set_last_error(NTE_BAD_FLAGS);
        return Ok(0);
    }

    let handle = process.new_crypto_provider(CryptoProviderState {
        provider_type,
        provider_name,
        container: None,
        flags,
    });
    if let Err(stop) = process.wrptr(provider_out, handle) {
        process.crypto_providers.remove(&handle);
        return Err(stop);
    }
    Ok(1)
}

fn crypt_create_hash(process: &mut Process) -> Result<u64, Stop> {
    const ERROR_INVALID_PARAMETER: u32 = 87;
    const NTE_BAD_UID: u32 = 0x8009_0001;
    const NTE_BAD_KEY: u32 = 0x8009_0003;
    const NTE_BAD_FLAGS: u32 = 0x8009_0009;
    const NTE_BAD_ALGID: u32 = 0x8009_0008;
    const CALG_MD5: u32 = 0x0000_8003;

    let (provider, algorithm, key, flags, hash_out) = (
        process.arg(0),
        process.arg(1) as u32,
        process.arg(2),
        process.arg(3) as u32,
        process.arg(4),
    );
    if !process.crypto_providers.contains_key(&provider) {
        process.set_last_error(NTE_BAD_UID);
        return Ok(0);
    }
    if hash_out == 0 {
        process.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if algorithm != CALG_MD5 {
        process.set_last_error(NTE_BAD_ALGID);
        return Ok(0);
    }
    if key != 0 {
        process.set_last_error(NTE_BAD_KEY);
        return Ok(0);
    }
    if flags != 0 {
        process.set_last_error(NTE_BAD_FLAGS);
        return Ok(0);
    }
    let handle = process.new_crypto_hash(CryptoHashState {
        provider,
        algorithm,
        data: Vec::new(),
        finalized: false,
    });
    if let Err(stop) = process.wrptr(hash_out, handle) {
        process.crypto_hashes.remove(&handle);
        return Err(stop);
    }
    Ok(1)
}

fn crypt_hash_data(process: &mut Process) -> Result<u64, Stop> {
    const NTE_BAD_HASH: u32 = 0x8009_0002;
    const NTE_BAD_HASH_STATE: u32 = 0x8009_000c;
    const NTE_BAD_FLAGS: u32 = 0x8009_0009;
    const NTE_NO_MEMORY: u32 = 0x8009_000e;
    const MAX_HASH_DATA_BYTES: usize = 256 << 20;

    let (hash, data_ptr, data_length, flags) = (
        process.arg(0),
        process.arg(1),
        process.arg(2) as u32 as usize,
        process.arg(3) as u32,
    );
    if flags != 0 {
        process.set_last_error(NTE_BAD_FLAGS);
        return Ok(0);
    }
    let Some(state) = process.crypto_hashes.get(&hash) else {
        process.set_last_error(NTE_BAD_HASH);
        return Ok(0);
    };
    if state.finalized {
        process.set_last_error(NTE_BAD_HASH_STATE);
        return Ok(0);
    }
    if data_length == 0 {
        return Ok(1);
    }
    if data_ptr == 0 {
        process.set_last_error(NTE_BAD_HASH);
        return Ok(0);
    }
    let Some(next_length) = state.data.len().checked_add(data_length) else {
        process.set_last_error(NTE_NO_MEMORY);
        return Ok(0);
    };
    if next_length > MAX_HASH_DATA_BYTES {
        process.set_last_error(NTE_NO_MEMORY);
        return Ok(0);
    }
    let bytes = process.read_bytes(data_ptr, data_length)?;
    let state = process
        .crypto_hashes
        .get_mut(&hash)
        .expect("hash checked above");
    if state.data.try_reserve(data_length).is_err() {
        process.set_last_error(NTE_NO_MEMORY);
        return Ok(0);
    }
    state.data.extend_from_slice(&bytes);
    Ok(1)
}

fn crypt_get_hash_param(process: &mut Process) -> Result<u64, Stop> {
    const ERROR_INVALID_PARAMETER: u32 = 87;
    const NTE_BAD_HASH: u32 = 0x8009_0002;
    const NTE_BAD_LEN: u32 = 0x8009_0004;
    const NTE_BAD_FLAGS: u32 = 0x8009_0009;
    const NTE_BAD_TYPE: u32 = 0x8009_000a;
    const CALG_MD5: u32 = 0x0000_8003;
    const HP_ALGID: u32 = 0x0000_0001;
    const HP_HASHVAL: u32 = 0x0000_0002;
    const HP_HASHSIZE: u32 = 0x0000_0004;

    let (hash, parameter, output, length_ptr, flags) = (
        process.arg(0),
        process.arg(1) as u32,
        process.arg(2),
        process.arg(3),
        process.arg(4) as u32,
    );
    if flags != 0 {
        process.set_last_error(NTE_BAD_FLAGS);
        return Ok(0);
    }
    let Some(state) = process.crypto_hashes.get(&hash) else {
        process.set_last_error(NTE_BAD_HASH);
        return Ok(0);
    };
    if length_ptr == 0 {
        process.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let value = match parameter {
        HP_ALGID => state.algorithm.to_le_bytes().to_vec(),
        HP_HASHSIZE => 16u32.to_le_bytes().to_vec(),
        HP_HASHVAL if state.algorithm == CALG_MD5 => Md5::digest(&state.data).to_vec(),
        HP_HASHVAL => {
            process.set_last_error(NTE_BAD_HASH);
            return Ok(0);
        }
        _ => {
            process.set_last_error(NTE_BAD_TYPE);
            return Ok(0);
        }
    };
    let required = value.len() as u32;
    if output == 0 {
        process.wr32(length_ptr, required)?;
        return Ok(1);
    }
    let capacity = process.rd32(length_ptr)?;
    process.wr32(length_ptr, required)?;
    if capacity < required {
        process.set_last_error(NTE_BAD_LEN);
        return Ok(0);
    }
    process.write_bytes(output, &value)?;
    if parameter == HP_HASHVAL {
        process
            .crypto_hashes
            .get_mut(&hash)
            .expect("hash checked above")
            .finalized = true;
    }
    Ok(1)
}

fn crypt_derive_key(process: &mut Process) -> Result<u64, Stop> {
    const ERROR_INVALID_PARAMETER: u32 = 87;
    const NTE_BAD_UID: u32 = 0x8009_0001;
    const NTE_BAD_HASH: u32 = 0x8009_0002;
    const NTE_BAD_HASH_STATE: u32 = 0x8009_000c;
    const NTE_BAD_FLAGS: u32 = 0x8009_0009;
    const NTE_BAD_ALGID: u32 = 0x8009_0008;
    const NTE_BAD_LEN: u32 = 0x8009_0004;
    const PROV_RSA_FULL: u32 = 1;
    const CALG_MD5: u32 = 0x0000_8003;
    const CALG_RC4: u32 = 0x0000_6801;
    const CRYPT_EXPORTABLE: u32 = 0x0000_0001;

    let (provider, algorithm, base_hash, flags, key_out) = (
        process.arg(0),
        process.arg(1) as u32,
        process.arg(2),
        process.arg(3) as u32,
        process.arg(4),
    );
    let Some(provider_state) = process.crypto_providers.get(&provider) else {
        process.set_last_error(NTE_BAD_UID);
        return Ok(0);
    };
    if provider_state.provider_type != PROV_RSA_FULL {
        process.set_last_error(NTE_BAD_UID);
        return Ok(0);
    }
    if key_out == 0 {
        process.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    if algorithm != CALG_RC4 {
        process.set_last_error(NTE_BAD_ALGID);
        return Ok(0);
    }
    let Some(hash) = process.crypto_hashes.get(&base_hash) else {
        process.set_last_error(NTE_BAD_HASH);
        return Ok(0);
    };
    if hash.provider != provider {
        process.set_last_error(NTE_BAD_HASH);
        return Ok(0);
    }
    if hash.finalized {
        process.set_last_error(NTE_BAD_HASH_STATE);
        return Ok(0);
    }
    if hash.algorithm != CALG_MD5 {
        process.set_last_error(NTE_BAD_ALGID);
        return Ok(0);
    }

    let low_flags = flags & 0xffff;
    if low_flags & !CRYPT_EXPORTABLE != 0 {
        process.set_last_error(NTE_BAD_FLAGS);
        return Ok(0);
    }
    let requested_bits = flags >> 16;
    let key_bits = if requested_bits == 0 {
        40
    } else {
        requested_bits
    };
    if !(40..=56).contains(&key_bits) || key_bits % 8 != 0 {
        process.set_last_error(NTE_BAD_LEN);
        return Ok(0);
    }

    let key_length = (key_bits / 8) as usize;
    let digest = Md5::digest(&hash.data);
    let key_material = digest[..key_length].to_vec();
    let Some(rc4) = Rc4State::new(&key_material) else {
        process.set_last_error(NTE_BAD_LEN);
        return Ok(0);
    };
    let handle = process.new_crypto_key(CryptoKeyState {
        provider,
        algorithm,
        key_material,
        exportable: low_flags & CRYPT_EXPORTABLE != 0,
        rc4: Some(rc4),
    });
    if let Err(stop) = process.wrptr(key_out, handle) {
        process.crypto_keys.remove(&handle);
        return Err(stop);
    }
    process
        .crypto_hashes
        .get_mut(&base_hash)
        .expect("hash validated above")
        .finalized = true;
    Ok(1)
}

fn crypt_encrypt(process: &mut Process) -> Result<u64, Stop> {
    crypt_rc4(process, true)
}

fn crypt_decrypt(process: &mut Process) -> Result<u64, Stop> {
    crypt_rc4(process, false)
}

fn crypt_rc4(process: &mut Process, encrypt: bool) -> Result<u64, Stop> {
    const ERROR_INVALID_PARAMETER: u32 = 87;
    const NTE_BAD_KEY: u32 = 0x8009_0003;
    const NTE_BAD_LEN: u32 = 0x8009_0004;
    const NTE_BAD_FLAGS: u32 = 0x8009_0009;
    const CALG_RC4: u32 = 0x0000_6801;

    let key = process.arg(0);
    let hash = process.arg(1);
    let _final_block = process.arg(2) != 0; // RC4 is a stream cipher and has no final padding.
    let flags = process.arg(3) as u32;
    let data_ptr = process.arg(4);
    let data_length_ptr = process.arg(5);
    let buffer_length = if encrypt {
        process.arg(6) as u32
    } else {
        u32::MAX
    };
    if flags != 0 {
        process.set_last_error(NTE_BAD_FLAGS);
        return Ok(0);
    }
    if hash != 0 {
        return Err(Stop::Unimplemented {
            dll: "advapi32".to_owned(),
            name: format!(
                "Crypt{} with an active hash handle is not translated",
                if encrypt { "Encrypt" } else { "Decrypt" }
            ),
        });
    }
    let Some(key_state) = process.crypto_keys.get(&key) else {
        process.set_last_error(NTE_BAD_KEY);
        return Ok(0);
    };
    if key_state.algorithm != CALG_RC4 {
        process.set_last_error(NTE_BAD_KEY);
        return Ok(0);
    }
    if data_length_ptr == 0 {
        process.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let data_length = process.rd32(data_length_ptr)? as usize;
    if encrypt && data_length > buffer_length as usize {
        process.set_last_error(NTE_BAD_LEN);
        return Ok(0);
    }
    if data_length != 0 && data_ptr == 0 {
        process.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let mut bytes = if data_length == 0 {
        Vec::new()
    } else {
        process.read_bytes(data_ptr, data_length)?
    };
    let key_state = process
        .crypto_keys
        .get_mut(&key)
        .expect("key checked above");
    let Some(rc4) = key_state.rc4.as_mut() else {
        process.set_last_error(NTE_BAD_KEY);
        return Ok(0);
    };
    rc4.apply(&mut bytes);
    if data_length != 0 {
        process.write_bytes(data_ptr, &bytes)?;
    }
    process.wr32(data_length_ptr, data_length as u32)?;
    Ok(1)
}

fn crypt_destroy_key(process: &mut Process) -> Result<u64, Stop> {
    const NTE_BAD_KEY: u32 = 0x8009_0003;
    if process.crypto_keys.remove(&process.arg(0)).is_some() {
        Ok(1)
    } else {
        process.set_last_error(NTE_BAD_KEY);
        Ok(0)
    }
}

fn crypt_destroy_hash(process: &mut Process) -> Result<u64, Stop> {
    const NTE_BAD_HASH: u32 = 0x8009_0002;
    if process.crypto_hashes.remove(&process.arg(0)).is_some() {
        Ok(1)
    } else {
        process.set_last_error(NTE_BAD_HASH);
        Ok(0)
    }
}

fn crypt_gen_random(process: &mut Process) -> Result<u64, Stop> {
    crypt_gen_random_with(process, fill_secure_random)
}

fn crypt_gen_random_with(
    process: &mut Process,
    fill_random: impl FnOnce(&mut [u8]) -> bool,
) -> Result<u64, Stop> {
    const ERROR_INVALID_PARAMETER: u32 = 87;
    const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
    const NTE_BAD_UID: u32 = 0x8009_0001;
    const NTE_FAIL: u32 = 0x8009_0004;

    let (provider, length, destination) = (
        process.arg(0),
        process.arg(1) as u32 as usize,
        process.arg(2),
    );
    if !process.crypto_providers.contains_key(&provider) {
        process.set_last_error(NTE_BAD_UID);
        return Ok(0);
    }
    if length != 0 && destination == 0 {
        process.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }
    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(length).is_err() {
        process.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
        return Ok(0);
    }
    bytes.resize(length, 0);
    if !fill_random(&mut bytes) {
        process.set_last_error(NTE_FAIL);
        return Ok(0);
    }
    if length != 0 {
        process.write_bytes(destination, &bytes)?;
    }
    Ok(1)
}

fn crypt_release_context(process: &mut Process) -> Result<u64, Stop> {
    const NTE_BAD_FLAGS: u32 = 0x8009_0009;
    const NTE_BAD_UID: u32 = 0x8009_0001;
    let (provider, flags) = (process.arg(0), process.arg(1) as u32);
    if flags != 0 {
        process.set_last_error(NTE_BAD_FLAGS);
        return Ok(0);
    }
    if process
        .crypto_hashes
        .values()
        .any(|hash| hash.provider == provider)
        || process
            .crypto_keys
            .values()
            .any(|key| key.provider == provider)
    {
        process.set_last_error(NTE_BAD_UID);
        return Ok(0);
    }
    if process.crypto_providers.remove(&provider).is_none() {
        process.set_last_error(NTE_BAD_UID);
        return Ok(0);
    }
    Ok(1)
}

fn get_user_name_a(process: &mut Process) -> Result<u64, Stop> {
    get_user_name(process, false)
}

fn get_user_name_w(process: &mut Process) -> Result<u64, Stop> {
    get_user_name(process, true)
}

fn get_user_name(process: &mut Process, wide: bool) -> Result<u64, Stop> {
    const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
    const ERROR_INVALID_PARAMETER: u32 = 87;

    let (buffer, size_pointer) = (process.arg(0), process.arg(1));
    if buffer == 0 || size_pointer == 0 {
        process.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(0);
    }

    let capacity = process.rd32(size_pointer)? as usize;
    let name: Vec<u16> = VIRTUAL_USER_NAME.encode_utf16().collect();
    let (bytes, required_characters) = if wide {
        let mut bytes = Vec::with_capacity((name.len() + 1) * 2);
        for unit in &name {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&[0, 0]);
        (bytes, name.len() + 1)
    } else {
        // The emulated Windows ACP is 1252, matching kernel32!GetACP.
        let mut bytes = crate::hle::kernel32::cp_encode(1252, &name);
        bytes.push(0);
        let required_characters = bytes.len();
        (bytes, required_characters)
    };

    if capacity < required_characters {
        process.wr32(size_pointer, required_characters as u32)?;
        process.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(0);
    }

    process.write_bytes(buffer, &bytes)?;
    process.wr32(size_pointer, required_characters as u32)?;
    Ok(1)
}

const PREDEFINED_ROOTS: [u32; 5] = [
    0x8000_0000, // HKEY_CLASSES_ROOT
    0x8000_0001, // HKEY_CURRENT_USER
    0x8000_0002, // HKEY_LOCAL_MACHINE
    0x8000_0003, // HKEY_USERS
    0x8000_0005, // HKEY_CURRENT_CONFIG
];

const KEY_QUERY_VALUE: u32 = 0x0001;
const KEY_SET_VALUE: u32 = 0x0002;
const KEY_CREATE_SUB_KEY: u32 = 0x0004;
const KEY_ENUMERATE_SUB_KEYS: u32 = 0x0008;
const KEY_ALL_ACCESS: u32 = 0x000f_003f;

fn is_predefined_root(key: u64) -> bool {
    PREDEFINED_ROOTS.contains(&(key as u32))
}

fn is_registry_handle(process: &Process, key: u64) -> bool {
    is_predefined_root(key)
        || process.registry_key_paths.contains_key(&key) && process.handles.contains_key(&key)
}

fn registry_handle_has_access(process: &Process, key: u64, required: u32) -> bool {
    is_predefined_root(key)
        || process
            .registry_key_access
            .get(&key)
            .map(|granted| granted & required == required)
            .unwrap_or(false)
}

fn create_registry_handle(
    process: &mut Process,
    path: String,
    access: u32,
    result_ptr: u64,
) -> Result<(), Stop> {
    let key = process.new_handle(Handle::Other);
    if let Err(stop) = process.wrptr(result_ptr, key) {
        process.handles.remove(&key);
        return Err(stop);
    }
    process.registry_key_paths.insert(key, path);
    process.registry_key_access.insert(key, access);
    Ok(())
}

fn registry_root_path(key: u64) -> Option<&'static str> {
    match key as u32 {
        0x8000_0000 => Some("hkey_classes_root"),
        0x8000_0001 => Some("hkey_current_user"),
        0x8000_0002 => Some("hkey_local_machine"),
        0x8000_0003 => Some("hkey_users"),
        0x8000_0005 => Some("hkey_current_config"),
        _ => None,
    }
}

fn registry_key_path(process: &Process, key: u64) -> Option<String> {
    registry_root_path(key)
        .map(str::to_owned)
        .or_else(|| process.registry_key_paths.get(&key).cloned())
}

fn append_registry_subkey(base: &str, subkey: &str) -> String {
    let subkey = subkey
        .split('\\')
        .filter(|part| !part.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join("\\");
    if subkey.is_empty() {
        base.to_owned()
    } else {
        format!("{base}\\{subkey}")
    }
}

fn registry_value_name(name: &str) -> String {
    name.to_lowercase()
}

fn registry_path_is_covered(process: &Process, path: &str) -> bool {
    process.registry_paths.contains(path)
        || process.registry_paths.iter().any(|known| {
            path.strip_prefix(known)
                .is_some_and(|suffix| suffix.starts_with('\\'))
        })
}

fn unsupported_registry(name: &str, detail: impl AsRef<str>) -> Stop {
    Stop::Unimplemented {
        dll: "advapi32".to_owned(),
        name: format!("{name}: {}", detail.as_ref()),
    }
}

fn registry_child_names(process: &Process, path: &str) -> Vec<String> {
    if let Some(children) = process.registry_children.get(path) {
        return children.clone();
    }
    let prefix = format!("{path}\\");
    let mut children = process
        .registry_paths
        .iter()
        .filter_map(|candidate| {
            let suffix = candidate.strip_prefix(&prefix)?;
            (!suffix.is_empty() && !suffix.contains('\\')).then(|| suffix.to_owned())
        })
        .collect::<Vec<_>>();
    children.sort_unstable();
    children
}

fn registry_value_bytes(
    process: &Process,
    path: &str,
    name: &str,
    wide_api: bool,
) -> Result<Option<(u32, Vec<u8>)>, Stop> {
    let key = (path.to_owned(), name.to_owned());
    let Some((value_type, bytes)) = process.registry_values.get(&key) else {
        return Ok(None);
    };
    const REG_SZ: u32 = 1;
    const REG_EXPAND_SZ: u32 = 2;
    const REG_MULTI_SZ: u32 = 7;
    if !matches!(*value_type, REG_SZ | REG_EXPAND_SZ | REG_MULTI_SZ) {
        return Ok(Some((*value_type, bytes.clone())));
    }
    let host_wide = process.registry_host_values.contains(&key);
    if host_wide == wide_api {
        return Ok(Some((*value_type, bytes.clone())));
    }
    if host_wide {
        if bytes.len() % 2 != 0 {
            return Err(unsupported_registry(
                if wide_api {
                    "RegQueryValueExW"
                } else {
                    "RegQueryValueExA"
                },
                format!("host registry string {name:?} has an odd UTF-16 byte length"),
            ));
        }
        let units = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        Ok(Some((
            *value_type,
            crate::hle::kernel32::cp_encode(1252, &units),
        )))
    } else {
        let units = bytes
            .iter()
            .map(|byte| crate::hle::kernel32::cp1252(*byte))
            .collect::<Vec<_>>();
        Ok(Some((
            *value_type,
            units.into_iter().flat_map(u16::to_le_bytes).collect(),
        )))
    }
}

/// Opens predefined roots and keys created in this process's volatile registry.
fn reg_open_key_ex_a(process: &mut Process) -> Result<u64, Stop> {
    let (root, subkey, options, access, result) = (
        process.arg(0),
        process.arg(1),
        process.arg(2) as u32 as u64,
        process.arg(3) as u32,
        process.arg(4),
    );
    open_key_a(
        process,
        root,
        subkey,
        options,
        access,
        result,
        "RegOpenKeyExA",
    )
}

fn reg_open_key_a(process: &mut Process) -> Result<u64, Stop> {
    let (root, subkey, result) = (process.arg(0), process.arg(1), process.arg(2));
    open_key_a(process, root, subkey, 0, 0x0002_0019, result, "RegOpenKeyA")
}

fn open_key_a(
    process: &mut Process,
    root: u64,
    subkey_ptr: u64,
    options: u64,
    access: u32,
    result_ptr: u64,
    api_name: &str,
) -> Result<u64, Stop> {
    const ERROR_SUCCESS: u64 = 0;
    const ERROR_FILE_NOT_FOUND: u64 = 2;
    const ERROR_INVALID_HANDLE: u64 = 6;
    const ERROR_INVALID_PARAMETER: u64 = 87;
    const REG_OPTION_OPEN_LINK: u64 = 8;
    let subkey = if subkey_ptr == 0 {
        String::new()
    } else {
        process.read_cstr(subkey_ptr)?
    };

    if process.trace_imports {
        process.stderr.extend_from_slice(
            format!(
                "[registry] {api_name} root={root:#x} subkey={subkey:?} options={options:#x} access={access:#x}\n"
            )
            .as_bytes(),
        );
    }

    if !is_registry_handle(process, root) {
        return Ok(ERROR_INVALID_HANDLE);
    }
    if result_ptr == 0 || (options != 0 && options != REG_OPTION_OPEN_LINK) {
        return Ok(ERROR_INVALID_PARAMETER);
    }
    let base = registry_key_path(process, root).expect("validated registry handle has a path");
    if !subkey.is_empty() && !registry_handle_has_access(process, root, KEY_ENUMERATE_SUB_KEYS) {
        return Ok(5); // ERROR_ACCESS_DENIED
    }
    let path = append_registry_subkey(&base, &subkey);
    if !process.registry_paths.contains(&path) && !(subkey.is_empty() && is_predefined_root(root)) {
        if registry_path_is_covered(process, &path) {
            return Ok(ERROR_FILE_NOT_FOUND);
        }
        return Err(unsupported_registry(
            api_name,
            format!("host registry path {path:?} is outside the supplied read-only snapshot"),
        ));
    }
    const DELETE: u32 = 0x0001_0000;
    const WRITE_DAC: u32 = 0x0004_0000;
    const WRITE_OWNER: u32 = 0x0008_0000;
    if process.registry_host_paths.contains(&path)
        && access & (KEY_SET_VALUE | KEY_CREATE_SUB_KEY | DELETE | WRITE_DAC | WRITE_OWNER) != 0
    {
        return Ok(5); // ERROR_ACCESS_DENIED: the host snapshot is read-only.
    }
    create_registry_handle(process, path, access, result_ptr)?;
    Ok(ERROR_SUCCESS)
}

fn reg_create_key_ex_a(process: &mut Process) -> Result<u64, Stop> {
    const ERROR_SUCCESS: u64 = 0;
    const ERROR_INVALID_HANDLE: u64 = 6;
    const ERROR_INVALID_PARAMETER: u64 = 87;
    const REG_OPTION_NON_VOLATILE: u64 = 0;
    const REG_OPTION_VOLATILE: u64 = 1;
    const REG_CREATED_NEW_KEY: u32 = 1;
    const REG_OPENED_EXISTING_KEY: u32 = 2;

    let (root, subkey_ptr, reserved, class_ptr, options, result_ptr, disposition_ptr) = (
        process.arg(0),
        process.arg(1),
        process.arg(2),
        process.arg(3),
        process.arg(4),
        process.arg(7),
        process.arg(8),
    );
    if !is_registry_handle(process, root) {
        return Ok(ERROR_INVALID_HANDLE);
    }
    if !registry_handle_has_access(process, root, KEY_CREATE_SUB_KEY) {
        return Ok(5); // ERROR_ACCESS_DENIED
    }
    if reserved != 0
        || !matches!(options, REG_OPTION_NON_VOLATILE | REG_OPTION_VOLATILE)
        || result_ptr == 0
    {
        return Ok(ERROR_INVALID_PARAMETER);
    }
    let subkey = if subkey_ptr == 0 {
        String::new()
    } else {
        process.read_cstr(subkey_ptr)?
    };
    let base = registry_key_path(process, root).expect("validated registry handle has a path");
    let path = append_registry_subkey(&base, &subkey);
    if process.registry_host_paths.contains(&base)
        || process.registry_host_paths.contains(&path)
        || path.starts_with("hkey_local_machine\\software\\sims(steam)\\")
        || path == "hkey_local_machine\\software\\sims(steam)"
    {
        return Ok(5); // ERROR_ACCESS_DENIED: host registry snapshots cannot be mutated.
    }
    if !registry_path_is_covered(process, &base) && !is_predefined_root(root) {
        return Err(unsupported_registry(
            "RegCreateKeyExA",
            format!("registry path {base:?} is not represented by the guest registry"),
        ));
    }
    let existed =
        path == base && is_predefined_root(root) || process.registry_paths.contains(&path);
    if process.trace_imports {
        process.stderr.extend_from_slice(
            format!(
                "[registry] RegCreateKeyExA root={root:#x} subkey={subkey:?} options={options:#x}\n"
            )
            .as_bytes(),
        );
    }
    let mut parent = base;
    for component in subkey.split('\\').filter(|part| !part.is_empty()) {
        let existed = process
            .registry_paths
            .contains(&format!("{parent}\\{}", component.to_lowercase()));
        let child_list = process.registry_children.entry(parent.clone()).or_default();
        if !existed {
            child_list.push(component.to_owned());
        }
        parent.push('\\');
        parent.push_str(&component.to_lowercase());
        process.registry_paths.insert(parent.clone());
        process.registry_children.entry(parent.clone()).or_default();
        process
            .registry_key_metadata
            .entry(parent.clone())
            .or_default();
    }
    let desired_access = process.arg(6) as u32;
    create_registry_handle(process, path, desired_access, result_ptr)?;
    if disposition_ptr != 0 {
        process.wr32(
            disposition_ptr,
            if existed {
                REG_OPENED_EXISTING_KEY
            } else {
                REG_CREATED_NEW_KEY
            },
        )?;
    }
    let _ = class_ptr;
    Ok(ERROR_SUCCESS)
}

fn reg_create_key_a(process: &mut Process) -> Result<u64, Stop> {
    const ERROR_SUCCESS: u64 = 0;
    const ERROR_INVALID_HANDLE: u64 = 6;
    const ERROR_INVALID_PARAMETER: u64 = 87;
    let (root, subkey_ptr, result_ptr) = (process.arg(0), process.arg(1), process.arg(2));
    if !is_registry_handle(process, root) {
        return Ok(ERROR_INVALID_HANDLE);
    }
    if result_ptr == 0 {
        return Ok(ERROR_INVALID_PARAMETER);
    }
    if !registry_handle_has_access(process, root, KEY_CREATE_SUB_KEY) {
        return Ok(5); // ERROR_ACCESS_DENIED
    }
    let subkey = if subkey_ptr == 0 {
        String::new()
    } else {
        process.read_cstr(subkey_ptr)?
    };
    let mut base = registry_key_path(process, root).expect("validated registry handle has a path");
    let path = append_registry_subkey(&base, &subkey);
    if process.registry_host_paths.contains(&base)
        || process.registry_host_paths.contains(&path)
        || path.starts_with("hkey_local_machine\\software\\sims(steam)\\")
        || path == "hkey_local_machine\\software\\sims(steam)"
    {
        return Ok(5); // ERROR_ACCESS_DENIED: the host registry snapshot is read-only.
    }
    if !registry_path_is_covered(process, &base) && !is_predefined_root(root) {
        return Err(unsupported_registry(
            "RegCreateKeyA",
            format!("registry path {base:?} is not represented by the guest registry"),
        ));
    }
    for component in subkey.split('\\').filter(|part| !part.is_empty()) {
        let next = format!("{}\\{}", base, component.to_lowercase());
        process.registry_paths.insert(next.clone());
        process.registry_children.entry(next.clone()).or_default();
        process
            .registry_key_metadata
            .entry(next.clone())
            .or_default();
        let children = process.registry_children.entry(base.clone()).or_default();
        if !children
            .iter()
            .any(|name| name.eq_ignore_ascii_case(component))
        {
            children.push(component.to_owned());
        }
        base = next;
    }
    create_registry_handle(process, path, KEY_ALL_ACCESS, result_ptr)?;
    Ok(ERROR_SUCCESS)
}

fn reg_set_value_ex_a(process: &mut Process) -> Result<u64, Stop> {
    const ERROR_SUCCESS: u64 = 0;
    const ERROR_INVALID_HANDLE: u64 = 6;
    const ERROR_INVALID_PARAMETER: u64 = 87;
    let key = process.arg(0);
    let value_name = process.arg(1);
    let reserved = process.arg(2);
    let value_type = process.arg(3) as u32;
    let data = process.arg(4);
    let size = process.arg(5) as u32 as usize;
    if !is_registry_handle(process, key) {
        return Ok(ERROR_INVALID_HANDLE);
    }
    if reserved != 0 || (size != 0 && data == 0) {
        return Ok(ERROR_INVALID_PARAMETER);
    }
    if !registry_handle_has_access(process, key, KEY_SET_VALUE) {
        return Ok(5); // ERROR_ACCESS_DENIED
    }
    let path = registry_key_path(process, key).expect("validated registry handle has a path");
    if process.registry_host_paths.contains(&path) {
        return Ok(5); // ERROR_ACCESS_DENIED: the host snapshot is read-only.
    }
    if !registry_path_is_covered(process, &path) {
        return Err(unsupported_registry(
            "RegSetValueExA",
            format!("registry path {path:?} is not represented by the guest registry"),
        ));
    }
    let name = if value_name == 0 {
        String::new()
    } else {
        registry_value_name(&process.read_cstr(value_name)?)
    };
    let bytes = if size == 0 {
        Vec::new()
    } else {
        process.read_bytes(data, size)?
    };
    let value_key = (path, name);
    process
        .registry_values
        .insert(value_key.clone(), (value_type, bytes));
    process.registry_host_values.remove(&value_key);
    Ok(ERROR_SUCCESS)
}

fn reg_set_value_ex_w(process: &mut Process) -> Result<u64, Stop> {
    const ERROR_SUCCESS: u64 = 0;
    const ERROR_INVALID_HANDLE: u64 = 6;
    const ERROR_INVALID_PARAMETER: u64 = 87;
    let (key, value_name, reserved, value_type, data, size) = (
        process.arg(0),
        process.arg(1),
        process.arg(2),
        process.arg(3) as u32,
        process.arg(4),
        process.arg(5) as u32 as usize,
    );
    if !is_registry_handle(process, key) {
        return Ok(ERROR_INVALID_HANDLE);
    }
    if reserved != 0 || (size != 0 && data == 0) {
        return Ok(ERROR_INVALID_PARAMETER);
    }
    if !registry_handle_has_access(process, key, KEY_SET_VALUE) {
        return Ok(5); // ERROR_ACCESS_DENIED
    }
    let path = registry_key_path(process, key).expect("validated registry handle has a path");
    if process.registry_host_paths.contains(&path) {
        return Ok(5); // ERROR_ACCESS_DENIED: the host snapshot is read-only.
    }
    if !registry_path_is_covered(process, &path) {
        return Err(unsupported_registry(
            "RegSetValueExW",
            format!("registry path {path:?} is not represented by the guest registry"),
        ));
    }
    let name = if value_name == 0 {
        String::new()
    } else {
        registry_value_name(&process.read_wstr(value_name)?)
    };
    let bytes = if size == 0 {
        Vec::new()
    } else {
        process.read_bytes(data, size)?
    };
    let value_key = (path, name);
    process
        .registry_values
        .insert(value_key.clone(), (value_type, bytes));
    if matches!(value_type, 1 | 2 | 7) {
        process.registry_host_values.insert(value_key);
    } else {
        process.registry_host_values.remove(&value_key);
    }
    Ok(ERROR_SUCCESS)
}

fn reg_query_value_ex_a(process: &mut Process) -> Result<u64, Stop> {
    const ERROR_SUCCESS: u64 = 0;
    const ERROR_FILE_NOT_FOUND: u64 = 2;
    const ERROR_INVALID_HANDLE: u64 = 6;
    const ERROR_INVALID_PARAMETER: u64 = 87;
    const ERROR_MORE_DATA: u64 = 234;
    let key = process.arg(0);
    let value_name = process.arg(1);
    let reserved = process.arg(2);
    let type_ptr = process.arg(3);
    let data_ptr = process.arg(4);
    let size_ptr = process.arg(5);
    if !is_registry_handle(process, key) {
        return Ok(ERROR_INVALID_HANDLE);
    }
    if reserved != 0 || (size_ptr == 0 && data_ptr != 0) {
        return Ok(ERROR_INVALID_PARAMETER);
    }
    if !registry_handle_has_access(process, key, KEY_QUERY_VALUE) {
        return Ok(5); // ERROR_ACCESS_DENIED
    }
    let path = registry_key_path(process, key).expect("validated registry handle has a path");
    let name = if value_name == 0 {
        String::new()
    } else {
        registry_value_name(&process.read_cstr(value_name)?)
    };
    if !registry_path_is_covered(process, &path) {
        return Err(unsupported_registry(
            "RegQueryValueExA",
            format!("registry path {path:?} is not represented by the guest registry"),
        ));
    }
    let Some((value_type, bytes)) = registry_value_bytes(process, &path, &name, false)? else {
        return Ok(ERROR_FILE_NOT_FOUND);
    };
    if type_ptr != 0 {
        process.wr32(type_ptr, value_type)?;
    }
    if data_ptr == 0 {
        if size_ptr != 0 {
            process.wr32(size_ptr, bytes.len() as u32)?;
        }
        return Ok(ERROR_SUCCESS);
    }

    let available = process.rd32(size_ptr)? as usize;
    if available < bytes.len() {
        process.wr32(size_ptr, bytes.len() as u32)?;
        return Ok(ERROR_MORE_DATA);
    }
    process.write_bytes(data_ptr, &bytes)?;
    process.wr32(size_ptr, bytes.len() as u32)?;
    Ok(ERROR_SUCCESS)
}

fn reg_query_value_ex_w(process: &mut Process) -> Result<u64, Stop> {
    query_value_ex(process, true)
}

fn query_value_ex(process: &mut Process, wide: bool) -> Result<u64, Stop> {
    const ERROR_SUCCESS: u64 = 0;
    const ERROR_FILE_NOT_FOUND: u64 = 2;
    const ERROR_INVALID_HANDLE: u64 = 6;
    const ERROR_INVALID_PARAMETER: u64 = 87;
    const ERROR_MORE_DATA: u64 = 234;
    let key = process.arg(0);
    let value_name = process.arg(1);
    let reserved = process.arg(2);
    let type_ptr = process.arg(3);
    let data_ptr = process.arg(4);
    let size_ptr = process.arg(5);
    if !is_registry_handle(process, key) {
        return Ok(ERROR_INVALID_HANDLE);
    }
    if reserved != 0 || (size_ptr == 0 && data_ptr != 0) {
        return Ok(ERROR_INVALID_PARAMETER);
    }
    if !registry_handle_has_access(process, key, KEY_QUERY_VALUE) {
        return Ok(5); // ERROR_ACCESS_DENIED
    }
    let path = registry_key_path(process, key).expect("validated registry handle has a path");
    if !registry_path_is_covered(process, &path) {
        return Err(unsupported_registry(
            if wide {
                "RegQueryValueExW"
            } else {
                "RegQueryValueExA"
            },
            format!("registry path {path:?} is not represented by the guest registry"),
        ));
    }
    let name = if value_name == 0 {
        String::new()
    } else if wide {
        registry_value_name(&process.read_wstr(value_name)?)
    } else {
        registry_value_name(&process.read_cstr(value_name)?)
    };
    let Some((value_type, bytes)) = registry_value_bytes(process, &path, &name, wide)? else {
        return Ok(ERROR_FILE_NOT_FOUND);
    };
    if type_ptr != 0 {
        process.wr32(type_ptr, value_type)?;
    }
    if data_ptr == 0 {
        if size_ptr != 0 {
            process.wr32(size_ptr, bytes.len() as u32)?;
        }
        return Ok(ERROR_SUCCESS);
    }
    let available = process.rd32(size_ptr)? as usize;
    if available < bytes.len() {
        process.wr32(size_ptr, bytes.len() as u32)?;
        return Ok(ERROR_MORE_DATA);
    }
    process.write_bytes(data_ptr, &bytes)?;
    process.wr32(size_ptr, bytes.len() as u32)?;
    Ok(ERROR_SUCCESS)
}

fn reg_query_info_key_a(process: &mut Process) -> Result<u64, Stop> {
    const ERROR_SUCCESS: u64 = 0;
    const ERROR_INVALID_HANDLE: u64 = 6;
    const ERROR_INVALID_PARAMETER: u64 = 87;
    const ERROR_MORE_DATA: u64 = 234;
    let key = process.arg(0);
    let class_ptr = process.arg(1);
    let class_len_ptr = process.arg(2);
    let reserved = process.arg(3);
    let subkeys_ptr = process.arg(4);
    let max_subkey_ptr = process.arg(5);
    let max_class_ptr = process.arg(6);
    let values_ptr = process.arg(7);
    let max_value_name_ptr = process.arg(8);
    let max_value_data_ptr = process.arg(9);
    let security_size_ptr = process.arg(10);
    let last_write_ptr = process.arg(11);
    if !is_registry_handle(process, key) {
        return Ok(ERROR_INVALID_HANDLE);
    }
    if reserved != 0 || class_ptr != 0 && class_len_ptr == 0 {
        return Ok(ERROR_INVALID_PARAMETER);
    }
    if !registry_handle_has_access(process, key, KEY_QUERY_VALUE) {
        return Ok(5); // ERROR_ACCESS_DENIED
    }
    let path = registry_key_path(process, key).expect("validated registry handle has a path");
    if !process.registry_paths.contains(&path) {
        return Err(unsupported_registry(
            "RegQueryInfoKeyA",
            format!("registry path {path:?} has no translated key metadata"),
        ));
    }
    let Some(metadata) = process.registry_key_metadata.get(&path).cloned() else {
        return Err(unsupported_registry(
            "RegQueryInfoKeyA",
            format!("registry key {path:?} has no translated class, security size, and timestamp"),
        ));
    };
    let children = registry_child_names(process, &path);
    let encoded_children = children
        .iter()
        .map(|name| crate::hle::kernel32::cp_encode(1252, &name.encode_utf16().collect::<Vec<_>>()))
        .collect::<Vec<_>>();
    let child_classes = children
        .iter()
        .filter_map(|name| {
            let child_path = format!("{path}\\{}", name.to_lowercase());
            process.registry_key_metadata.get(&child_path)
        })
        .map(|metadata| {
            crate::hle::kernel32::cp_encode(
                1252,
                &metadata.class_name.encode_utf16().collect::<Vec<_>>(),
            )
            .len()
        })
        .max()
        .unwrap_or(0);
    let (value_count, max_value_name, max_value_data) = process
        .registry_values
        .iter()
        .filter(|((value_path, _), _)| value_path == &path)
        .fold(
            (0usize, 0usize, 0usize),
            |(count, max_name, max_data), ((_, name), (_, data))| {
                (
                    count + 1,
                    max_name.max(
                        crate::hle::kernel32::cp_encode(
                            1252,
                            &name.encode_utf16().collect::<Vec<_>>(),
                        )
                        .len(),
                    ),
                    max_data.max(data.len()),
                )
            },
        );
    if subkeys_ptr != 0 {
        process.wr32(subkeys_ptr, children.len() as u32)?;
    }
    if max_subkey_ptr != 0 {
        process.wr32(
            max_subkey_ptr,
            encoded_children.iter().map(Vec::len).max().unwrap_or(0) as u32,
        )?;
    }
    if max_class_ptr != 0 {
        process.wr32(max_class_ptr, child_classes as u32)?;
    }
    if values_ptr != 0 {
        process.wr32(values_ptr, value_count as u32)?;
    }
    if max_value_name_ptr != 0 {
        process.wr32(max_value_name_ptr, max_value_name as u32)?;
    }
    if max_value_data_ptr != 0 {
        process.wr32(max_value_data_ptr, max_value_data as u32)?;
    }
    if security_size_ptr != 0 {
        let Some(size) = metadata.security_descriptor_size else {
            return Err(unsupported_registry(
                "RegQueryInfoKeyA",
                "security descriptor size was not captured",
            ));
        };
        process.wr32(security_size_ptr, size)?;
    }
    if last_write_ptr != 0 {
        let Some(filetime) = metadata.last_write_filetime else {
            return Err(unsupported_registry(
                "RegQueryInfoKeyA",
                "last-write time was not captured",
            ));
        };
        process.wr64(last_write_ptr, filetime)?;
    }

    let class = crate::hle::kernel32::cp_encode(
        1252,
        &metadata.class_name.encode_utf16().collect::<Vec<_>>(),
    );
    if class_ptr != 0 {
        let available = process.rd32(class_len_ptr)? as usize;
        if available < class.len() + 1 {
            process.wr32(class_len_ptr, class.len() as u32)?;
            return Ok(ERROR_MORE_DATA);
        }
        let mut output = class;
        output.push(0);
        process.write_bytes(class_ptr, &output)?;
        process.wr32(class_len_ptr, (output.len() - 1) as u32)?;
    } else if class_len_ptr != 0 {
        process.wr32(class_len_ptr, class.len() as u32)?;
    }
    Ok(ERROR_SUCCESS)
}

fn reg_enum_key_ex_a(process: &mut Process) -> Result<u64, Stop> {
    const ERROR_SUCCESS: u64 = 0;
    const ERROR_INVALID_HANDLE: u64 = 6;
    const ERROR_INVALID_PARAMETER: u64 = 87;
    const ERROR_MORE_DATA: u64 = 234;
    const ERROR_NO_MORE_ITEMS: u64 = 259;
    let key = process.arg(0);
    let index = process.arg(1) as u32 as usize;
    let name_ptr = process.arg(2);
    let name_len_ptr = process.arg(3);
    let reserved = process.arg(4);
    let class_ptr = process.arg(5);
    let class_len_ptr = process.arg(6);
    let last_write_ptr = process.arg(7);
    if !is_registry_handle(process, key) {
        return Ok(ERROR_INVALID_HANDLE);
    }
    if reserved != 0 || name_ptr == 0 || name_len_ptr == 0 || class_ptr != 0 && class_len_ptr == 0 {
        return Ok(ERROR_INVALID_PARAMETER);
    }
    if !registry_handle_has_access(process, key, KEY_ENUMERATE_SUB_KEYS) {
        return Ok(5); // ERROR_ACCESS_DENIED
    }
    let path = registry_key_path(process, key).expect("validated registry handle has a path");
    if !process.registry_paths.contains(&path) {
        return Err(unsupported_registry(
            "RegEnumKeyExA",
            format!("registry path {path:?} has no translated subkey list"),
        ));
    }
    let children = registry_child_names(process, &path);
    let Some(name) = children.get(index) else {
        return Ok(ERROR_NO_MORE_ITEMS);
    };
    let child_path = format!("{path}\\{}", name.to_lowercase());
    let metadata = process.registry_key_metadata.get(&child_path).cloned();
    if last_write_ptr != 0
        && metadata
            .as_ref()
            .and_then(|m| m.last_write_filetime)
            .is_none()
    {
        return Err(unsupported_registry(
            "RegEnumKeyExA",
            format!("subkey {child_path:?} has no translated last-write timestamp"),
        ));
    }
    let name_bytes =
        crate::hle::kernel32::cp_encode(1252, &name.encode_utf16().collect::<Vec<_>>());
    let class_bytes = metadata
        .as_ref()
        .map(|metadata| {
            crate::hle::kernel32::cp_encode(
                1252,
                &metadata.class_name.encode_utf16().collect::<Vec<_>>(),
            )
        })
        .unwrap_or_default();
    let name_capacity = process.rd32(name_len_ptr)? as usize;
    let class_capacity = if class_len_ptr == 0 {
        0
    } else {
        process.rd32(class_len_ptr)? as usize
    };
    let name_too_small = name_capacity < name_bytes.len() + 1;
    let class_too_small = class_ptr != 0 && class_capacity < class_bytes.len() + 1;
    if name_too_small {
        process.wr32(name_len_ptr, name_bytes.len() as u32)?;
    }
    if class_too_small {
        process.wr32(class_len_ptr, class_bytes.len() as u32)?;
    }
    if name_too_small || class_too_small {
        return Ok(ERROR_MORE_DATA);
    }
    let mut name_output = name_bytes;
    name_output.push(0);
    process.write_bytes(name_ptr, &name_output)?;
    process.wr32(name_len_ptr, (name_output.len() - 1) as u32)?;
    if class_ptr != 0 {
        let mut class_output = class_bytes;
        class_output.push(0);
        process.write_bytes(class_ptr, &class_output)?;
        process.wr32(class_len_ptr, (class_output.len() - 1) as u32)?;
    }
    if last_write_ptr != 0 {
        process.wr64(
            last_write_ptr,
            metadata
                .expect("timestamp checked above")
                .last_write_filetime
                .expect("timestamp checked above"),
        )?;
    }
    Ok(ERROR_SUCCESS)
}

fn reg_close_key(process: &mut Process) -> Result<u64, Stop> {
    const ERROR_SUCCESS: u64 = 0;
    const ERROR_INVALID_HANDLE: u64 = 6;
    let key = process.arg(0);
    if is_predefined_root(key) {
        return Ok(ERROR_SUCCESS);
    }
    if process.registry_key_paths.remove(&key).is_some() && process.handles.remove(&key).is_some() {
        process.registry_key_access.remove(&key);
        Ok(ERROR_SUCCESS)
    } else {
        Ok(ERROR_INVALID_HANDLE)
    }
}

fn system_function_036(process: &mut Process) -> Result<u64, Stop> {
    system_function_036_with(process, fill_secure_random)
}

fn system_function_036_with(
    process: &mut Process,
    fill_random: impl FnOnce(&mut [u8]) -> bool,
) -> Result<u64, Stop> {
    let destination = process.arg(0);
    let length = process.arg(1) as u32 as usize;
    if length == 0 {
        return Ok(1);
    }

    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(length).is_err() {
        return Ok(0);
    }
    bytes.resize(length, 0);
    if !fill_random(&mut bytes) {
        return Ok(0);
    }

    process.write_bytes(destination, &bytes)?;
    Ok(1)
}

#[cfg(windows)]
fn fill_secure_random(bytes: &mut [u8]) -> bool {
    // BCRYPT_USE_SYSTEM_PREFERRED_RNG
    unsafe {
        BCryptGenRandom(
            core::ptr::null_mut(),
            bytes.as_mut_ptr(),
            bytes.len() as u32,
            2,
        ) == 0
    }
}

#[cfg(all(not(windows), not(target_arch = "wasm32")))]
fn fill_secure_random(bytes: &mut [u8]) -> bool {
    use std::io::Read;

    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(bytes))
        .is_ok()
}

#[cfg(target_arch = "wasm32")]
fn fill_secure_random(bytes: &mut [u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }
    unsafe { anywasm_random_fill(bytes.as_mut_ptr(), bytes.len() as u32) == 1 }
}

#[cfg(test)]
mod tests {
    use super::{
        crypt_gen_random_with, lookup, system_function_036_with, ERROR_INSUFFICIENT_BUFFER,
        ERROR_INVALID_PARAMETER,
    };
    use crate::process::{
        CryptoHashState, CryptoKeyState, CryptoProviderState, Handle, Process, Rc4State,
        RegistryKeyMetadata, TEB_ADDR,
    };
    use crate::vfs::Vfs;
    use crate::Stop;
    use anywasm_cpu::cpu::{R8, R9, RCX, RDX, RSP};
    use anywasm_cpu::mem::{PAGE_SIZE, PROT_RW};

    const BASE: u64 = 0x10000;

    #[test]
    fn get_user_name_a_reports_virtual_identity_and_required_buffer_size() {
        let mut process = Process::new(Vfs::new());
        assert!(process.mem.map(BASE, PAGE_SIZE, PROT_RW));
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        process.cpu.mode = anywasm_cpu::cpu::CpuMode::X86_64;
        let buffer = BASE + 0x100;
        let size = BASE + 0x200;
        process.write_bytes(buffer, &[0xcc; 16]).unwrap();
        process.wr32(size, 7).unwrap();

        let get_user_name = lookup("GetUserNameA").expect("GetUserNameA should be implemented");
        process.cpu.gpr[RCX] = buffer;
        process.cpu.gpr[RDX] = size;
        assert_eq!(get_user_name(&mut process), Ok(0));
        assert_eq!(process.rd32(size), Ok(8));
        assert_eq!(process.rd32(TEB_ADDR + 0x68), Ok(122)); // ERROR_INSUFFICIENT_BUFFER

        process.wr32(size, 8).unwrap();
        process.set_last_error(0x1234_5678);
        assert_eq!(get_user_name(&mut process), Ok(1));
        assert_eq!(process.read_bytes(buffer, 8).unwrap(), b"AnyWASM\0");
        assert_eq!(process.rd32(size), Ok(8)); // Includes the terminating NUL.
        assert_eq!(process.rd32(TEB_ADDR + 0x68), Ok(0x1234_5678));
    }

    #[test]
    fn get_user_name_w_counts_wide_characters_and_rejects_null_parameters() {
        let mut process = Process::new(Vfs::new());
        assert!(process.mem.map(BASE, PAGE_SIZE, PROT_RW));
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        process.cpu.mode = anywasm_cpu::cpu::CpuMode::X86_64;
        let buffer = BASE + 0x100;
        let size = BASE + 0x200;
        let get_user_name = lookup("GetUserNameW").expect("GetUserNameW should be implemented");

        process.wr32(size, 8).unwrap();
        process.cpu.gpr[RCX] = buffer;
        process.cpu.gpr[RDX] = size;
        assert_eq!(get_user_name(&mut process), Ok(1));
        assert_eq!(process.read_wstr(buffer).unwrap(), "AnyWASM");
        assert_eq!(process.rd32(size), Ok(8));

        process.set_last_error(0);
        process.cpu.gpr[RCX] = 0;
        assert_eq!(get_user_name(&mut process), Ok(0));
        assert_eq!(process.rd32(TEB_ADDR + 0x68), Ok(87)); // ERROR_INVALID_PARAMETER

        process.set_last_error(0);
        process.cpu.gpr[RCX] = buffer;
        process.cpu.gpr[RDX] = 0;
        assert_eq!(get_user_name(&mut process), Ok(0));
        assert_eq!(process.rd32(TEB_ADDR + 0x68), Ok(87)); // ERROR_INVALID_PARAMETER
    }

    fn process_with_buffer(initial: u8) -> (Process, u64) {
        let mut process = Process::new(Vfs::new());
        assert!(process.mem.map(BASE, PAGE_SIZE, PROT_RW));
        let buffer = BASE + 0x100;
        process.write_bytes(buffer, &[initial; 4]).unwrap();
        process.cpu.gpr[RCX] = buffer;
        process.cpu.gpr[RDX] = 4;
        (process, buffer)
    }

    #[test]
    fn zero_length_request_succeeds_without_requesting_entropy() {
        let mut process = Process::new(Vfs::new());
        process.cpu.gpr[RCX] = 0;
        process.cpu.gpr[RDX] = 0;

        assert_eq!(
            system_function_036_with(&mut process, |_: &mut [u8]| {
                panic!("zero-length request used entropy")
            }),
            Ok(1)
        );
    }

    #[test]
    fn entropy_failure_returns_false_without_changing_guest_buffer() {
        let (mut process, buffer) = process_with_buffer(0xaa);

        assert_eq!(
            system_function_036_with(&mut process, |bytes: &mut [u8]| {
                bytes.fill(0x55);
                false
            }),
            Ok(0)
        );
        assert_eq!(process.read_bytes(buffer, 4).unwrap(), [0xaa; 4]);
    }

    #[test]
    fn successful_entropy_is_copied_to_guest_buffer() {
        let (mut process, buffer) = process_with_buffer(0xaa);

        assert_eq!(
            system_function_036_with(&mut process, |bytes: &mut [u8]| {
                bytes.fill(0x3c);
                true
            }),
            Ok(1)
        );
        assert_eq!(process.read_bytes(buffer, 4).unwrap(), [0x3c; 4]);
    }

    fn registry_process(root: u64, subkey: Option<&str>) -> (Process, u64) {
        let mut process = Process::new(Vfs::new());
        assert!(process.mem.map(BASE, PAGE_SIZE, PROT_RW));
        let stack = BASE + 0x800;
        let result = BASE + 0x200;
        process
            .write_bytes(result, &0xfeed_face_cafe_beefu64.to_le_bytes())
            .unwrap();
        process.cpu.gpr[RCX] = root;
        process.cpu.gpr[RDX] = match subkey {
            Some(subkey) => {
                let address = BASE + 0x100;
                process.write_cstr(address, subkey).unwrap();
                address
            }
            None => 0,
        };
        process.cpu.gpr[R8] = 0;
        process.cpu.gpr[R9] = 0x20019;
        process.cpu.gpr[RSP] = stack;
        process.wr64(stack + 40, result).unwrap();
        (process, result)
    }

    fn registry_x86_call(args: &[u32]) -> Process {
        let mut process = Process::new(Vfs::new());
        assert!(process.mem.map(BASE, PAGE_SIZE, PROT_RW));
        process.cpu.mode = anywasm_cpu::cpu::CpuMode::X86_32;
        let stack = BASE + 0x800;
        process.cpu.gpr[RSP] = stack;
        process.wr32(stack, 0xdead_beef).unwrap(); // return address
        for (index, arg) in args.iter().copied().enumerate() {
            process.wr32(stack + 4 + index as u64 * 4, arg).unwrap();
        }
        process
    }

    fn set_registry_x86_args(process: &mut Process, args: &[u32]) {
        let stack = process.cpu.gpr[RSP];
        process.wr32(stack, 0xdead_beef).unwrap();
        for (index, arg) in args.iter().copied().enumerate() {
            process.wr32(stack + 4 + index as u64 * 4, arg).unwrap();
        }
    }

    #[test]
    fn sid_helpers_allocate_copy_measure_and_free_owned_sid_storage() {
        let mut process = registry_x86_call(&[]);
        let authority = BASE + 0x100;
        let output = BASE + 0x120;
        let destination = BASE + 0x180;
        process.write_bytes(authority, &[0, 0, 0, 0, 0, 5]).unwrap();
        set_registry_x86_args(
            &mut process,
            &[
                authority as u32,
                2,
                21,
                1001,
                0,
                0,
                0,
                0,
                0,
                0,
                output as u32,
            ],
        );
        let allocate = lookup("AllocateAndInitializeSid").unwrap();
        assert_eq!(allocate(&mut process), Ok(1));
        let sid = process.rd32(output).unwrap() as u64;
        assert_ne!(sid, 0);
        assert_eq!(
            process.read_bytes(sid, 16).unwrap(),
            [1, 2, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0, 0xe9, 3, 0, 0]
        );

        set_registry_x86_args(&mut process, &[sid as u32]);
        assert_eq!(lookup("GetLengthSid").unwrap()(&mut process), Ok(16));
        set_registry_x86_args(&mut process, &[16, destination as u32, sid as u32]);
        assert_eq!(lookup("CopySid").unwrap()(&mut process), Ok(1));
        assert_eq!(
            process.read_bytes(destination, 16).unwrap(),
            process.read_bytes(sid, 16).unwrap()
        );

        set_registry_x86_args(&mut process, &[sid as u32]);
        assert_eq!(lookup("FreeSid").unwrap()(&mut process), Ok(0));
        assert!(process.extra.advapi32.sid_allocations.is_empty());
        assert_eq!(process.heap.size_of(sid), None);
        set_registry_x86_args(&mut process, &[sid as u32]);
        assert_eq!(lookup("FreeSid").unwrap()(&mut process), Ok(sid));
    }

    #[test]
    fn sid_helpers_reject_short_destination_and_malformed_sid_without_writes() {
        let mut process = registry_x86_call(&[]);
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        let sid = BASE + 0x100;
        let destination = BASE + 0x180;
        process
            .write_bytes(sid, &[1, 1, 0, 0, 0, 5, 18, 0, 0, 0])
            .unwrap();
        process.write_bytes(destination, &[0xcc; 16]).unwrap();
        set_registry_x86_args(&mut process, &[8, destination as u32, sid as u32]);
        assert_eq!(lookup("CopySid").unwrap()(&mut process), Ok(0));
        assert_eq!(process.rd32(TEB_ADDR + 0x34), Ok(ERROR_INSUFFICIENT_BUFFER));
        assert_eq!(process.read_bytes(destination, 16).unwrap(), [0xcc; 16]);

        process.wr8(sid, 2).unwrap();
        set_registry_x86_args(&mut process, &[sid as u32]);
        assert_eq!(lookup("GetLengthSid").unwrap()(&mut process), Ok(0));
        set_registry_x86_args(&mut process, &[16, destination as u32, sid as u32]);
        assert_eq!(lookup("CopySid").unwrap()(&mut process), Ok(0));
        assert_eq!(process.rd32(TEB_ADDR + 0x34), Ok(ERROR_INVALID_PARAMETER));
        assert_eq!(process.read_bytes(destination, 16).unwrap(), [0xcc; 16]);
    }

    #[test]
    fn build_trustee_with_sid_w_sets_the_documented_sid_form_fields() {
        let mut process = registry_x86_call(&[]);
        let trustee = BASE + 0x100;
        let sid = BASE + 0x200;
        process.write_bytes(trustee, &[0xcc; 20]).unwrap();
        process.write_bytes(sid, &[1, 0, 0, 0, 0, 0, 0, 5]).unwrap();
        set_registry_x86_args(&mut process, &[trustee as u32, sid as u32]);

        assert_eq!(lookup("BuildTrusteeWithSidW").unwrap()(&mut process), Ok(0));
        assert_eq!(process.rd32(trustee), Ok(0)); // pMultipleTrustee
        assert_eq!(process.rd32(trustee + 4), Ok(0)); // NO_MULTIPLE_TRUSTEE
        assert_eq!(process.rd32(trustee + 8), Ok(0)); // TRUSTEE_IS_SID
        assert_eq!(process.rd32(trustee + 12), Ok(0)); // TRUSTEE_IS_UNKNOWN
        assert_eq!(process.rd32(trustee + 16), Ok(sid as u32));
    }

    #[test]
    fn sid_group_resolves_generated_x86_stack_cleanup_for_each_handler() {
        for name in [
            "AllocateAndInitializeSid",
            "BuildTrusteeWithSidW",
            "CopySid",
            "FreeSid",
            "GetLengthSid",
        ] {
            assert!(lookup(name).is_some(), "{name} must be registered");
            assert!(
                crate::x86_abi_generated::cleanup_bytes("advapi32", name).is_some(),
                "{name} must have generated cleanup metadata"
            );
        }
    }

    fn token_test_process() -> Process {
        let mut process = registry_x86_call(&[]);
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        process
    }

    fn seed_test_identity(process: &mut Process) -> Vec<u8> {
        let sid = vec![1, 1, 0, 0, 0, 0, 0, 5, 42, 0, 0, 0];
        process.extra.advapi32.process_identity = Some(super::TokenIdentity {
            sid: sid.clone(),
            account_name: "FixtureUser".into(),
            domain_name: "FixtureDomain".into(),
            sid_name_use: super::SID_TYPE_USER,
        });
        sid
    }

    #[test]
    fn process_token_and_token_user_require_an_authoritative_identity() {
        const TOKEN_QUERY: u32 = 0x0008;
        let mut process = token_test_process();
        let token_out = BASE + 0x100;
        set_registry_x86_args(&mut process, &[u32::MAX, TOKEN_QUERY, token_out as u32]);
        assert_eq!(lookup("OpenProcessToken").unwrap()(&mut process), Ok(1));
        let token = process.rd32(token_out).unwrap() as u64;
        assert!(process.extra.advapi32.tokens.contains_key(&token));

        let info = BASE + 0x200;
        let length_out = BASE + 0x180;
        process.write_bytes(info, &[0xcc; 24]).unwrap();
        set_registry_x86_args(
            &mut process,
            &[token as u32, 1, info as u32, 24, length_out as u32],
        );
        assert_eq!(lookup("GetTokenInformation").unwrap()(&mut process), Ok(0));
        assert_eq!(process.rd32(TEB_ADDR + 0x34), Ok(50)); // ERROR_NOT_SUPPORTED
        assert_eq!(process.rd32(length_out), Ok(0));
        assert_eq!(process.read_bytes(info, 24).unwrap(), [0xcc; 24]);
    }

    #[test]
    fn token_user_size_query_and_lookup_account_sid_w_write_documented_x86_data() {
        const TOKEN_QUERY: u32 = 0x0008;
        const TOKEN_DUPLICATE: u32 = 0x0002;
        let mut process = token_test_process();
        let sid = seed_test_identity(&mut process);
        let token_out = BASE + 0x100;
        set_registry_x86_args(
            &mut process,
            &[u32::MAX, TOKEN_QUERY | TOKEN_DUPLICATE, token_out as u32],
        );
        assert_eq!(lookup("OpenProcessToken").unwrap()(&mut process), Ok(1));
        let token = process.rd32(token_out).unwrap();
        let return_length = BASE + 0x180;
        set_registry_x86_args(&mut process, &[token, 1, 0, 0, return_length as u32]);
        assert_eq!(lookup("GetTokenInformation").unwrap()(&mut process), Ok(0));
        assert_eq!(process.rd32(return_length), Ok(20));
        assert_eq!(process.rd32(TEB_ADDR + 0x34), Ok(ERROR_INSUFFICIENT_BUFFER));

        let info = BASE + 0x200;
        process.write_bytes(info, &[0xcc; 24]).unwrap();
        set_registry_x86_args(
            &mut process,
            &[token, 1, info as u32, 19, return_length as u32],
        );
        assert_eq!(lookup("GetTokenInformation").unwrap()(&mut process), Ok(0));
        assert_eq!(process.read_bytes(info, 24).unwrap(), [0xcc; 24]);
        assert_eq!(process.rd32(return_length), Ok(20));

        set_registry_x86_args(
            &mut process,
            &[token, 1, info as u32, 20, return_length as u32],
        );
        assert_eq!(lookup("GetTokenInformation").unwrap()(&mut process), Ok(1));
        let sid_ptr = process.rd32(info).unwrap() as u64;
        assert_eq!(sid_ptr, info + 8);
        assert_eq!(process.rd32(info + 4), Ok(0));
        assert_eq!(process.read_bytes(sid_ptr, sid.len()).unwrap(), sid);
        assert_eq!(process.rd32(return_length), Ok(20));

        let name_len = BASE + 0x300;
        let domain_len = BASE + 0x304;
        let sid_use = BASE + 0x308;
        process.wr32(name_len, 0).unwrap();
        process.wr32(domain_len, 0).unwrap();
        set_registry_x86_args(
            &mut process,
            &[
                0,
                sid_ptr as u32,
                0,
                name_len as u32,
                0,
                domain_len as u32,
                sid_use as u32,
            ],
        );
        assert_eq!(lookup("LookupAccountSidW").unwrap()(&mut process), Ok(0));
        assert_eq!(process.rd32(TEB_ADDR + 0x34), Ok(ERROR_INSUFFICIENT_BUFFER));
        assert_eq!(process.rd32(name_len), Ok(12));
        assert_eq!(process.rd32(domain_len), Ok(14));
        assert_eq!(process.rd32(sid_use), Ok(1));

        let name = BASE + 0x400;
        let domain = BASE + 0x480;
        process.wr32(name_len, 24).unwrap();
        process.wr32(domain_len, 24).unwrap();
        set_registry_x86_args(
            &mut process,
            &[
                0,
                sid_ptr as u32,
                name as u32,
                name_len as u32,
                domain as u32,
                domain_len as u32,
                sid_use as u32,
            ],
        );
        assert_eq!(lookup("LookupAccountSidW").unwrap()(&mut process), Ok(1));
        assert_eq!(process.read_wstr(name).unwrap(), "FixtureUser");
        assert_eq!(process.read_wstr(domain).unwrap(), "FixtureDomain");
        assert_eq!(process.rd32(name_len), Ok(12));
        assert_eq!(process.rd32(domain_len), Ok(14));
        assert_eq!(process.rd32(sid_use), Ok(1));
    }

    #[test]
    fn token_duplication_impersonation_open_thread_token_and_revert_track_thread_state() {
        const TOKEN_QUERY: u32 = 0x0008;
        const TOKEN_DUPLICATE: u32 = 0x0002;
        const TOKEN_IMPERSONATE: u32 = 0x0004;
        const SECURITY_IDENTIFICATION: u32 = 1;
        const SECURITY_IMPERSONATION: u32 = 2;
        let mut process = token_test_process();
        let sid = seed_test_identity(&mut process);
        let primary_out = BASE + 0x100;
        set_registry_x86_args(
            &mut process,
            &[u32::MAX, TOKEN_QUERY | TOKEN_DUPLICATE, primary_out as u32],
        );
        assert_eq!(lookup("OpenProcessToken").unwrap()(&mut process), Ok(1));
        let primary = process.rd32(primary_out).unwrap();

        let duplicate_out = BASE + 0x104;
        set_registry_x86_args(
            &mut process,
            &[primary, SECURITY_IDENTIFICATION, duplicate_out as u32],
        );
        assert_eq!(lookup("DuplicateToken").unwrap()(&mut process), Ok(1));
        let identification = process.rd32(duplicate_out).unwrap();
        set_registry_x86_args(&mut process, &[identification]);
        assert_eq!(
            lookup("ImpersonateLoggedOnUser").unwrap()(&mut process),
            Ok(0)
        );
        assert_eq!(process.rd32(TEB_ADDR + 0x34), Ok(5)); // level is below impersonation
        assert!(!process
            .extra
            .advapi32
            .thread_tokens
            .contains_key(&process.current_thread_id));

        set_registry_x86_args(
            &mut process,
            &[primary, SECURITY_IMPERSONATION, duplicate_out as u32],
        );
        assert_eq!(lookup("DuplicateToken").unwrap()(&mut process), Ok(1));
        let duplicate = process.rd32(duplicate_out).unwrap();
        let thread_handle_out = BASE + 0x108;
        set_registry_x86_args(&mut process, &[duplicate]);
        assert_eq!(
            lookup("ImpersonateLoggedOnUser").unwrap()(&mut process),
            Ok(1)
        );
        assert_eq!(
            process.extra.advapi32.thread_tokens[&process.current_thread_id]
                .identity
                .as_ref()
                .unwrap()
                .sid,
            sid
        );

        set_registry_x86_args(
            &mut process,
            &[
                (u32::MAX - 1),
                TOKEN_QUERY | TOKEN_IMPERSONATE,
                1,
                thread_handle_out as u32,
            ],
        );
        assert_eq!(lookup("OpenThreadToken").unwrap()(&mut process), Ok(1));
        let thread_token = process.rd32(thread_handle_out).unwrap();
        let info = BASE + 0x200;
        let return_length = BASE + 0x180;
        set_registry_x86_args(
            &mut process,
            &[thread_token, 1, info as u32, 20, return_length as u32],
        );
        assert_eq!(lookup("GetTokenInformation").unwrap()(&mut process), Ok(1));
        assert_eq!(process.read_bytes(info + 8, sid.len()).unwrap(), sid);

        set_registry_x86_args(&mut process, &[]);
        assert_eq!(lookup("RevertToSelf").unwrap()(&mut process), Ok(1));
        assert!(!process
            .extra
            .advapi32
            .thread_tokens
            .contains_key(&process.current_thread_id));
        set_registry_x86_args(
            &mut process,
            &[(u32::MAX - 1), TOKEN_QUERY, 1, thread_handle_out as u32],
        );
        assert_eq!(lookup("OpenThreadToken").unwrap()(&mut process), Ok(0));
        assert_eq!(process.rd32(TEB_ADDR + 0x34), Ok(1008)); // ERROR_NO_TOKEN

        set_registry_x86_args(
            &mut process,
            &[duplicate, SECURITY_IMPERSONATION, duplicate_out as u32],
        );
        assert_eq!(lookup("DuplicateToken").unwrap()(&mut process), Ok(0));
        assert_eq!(process.rd32(TEB_ADDR + 0x34), Ok(5)); // duplicate handle lacks TOKEN_DUPLICATE
    }

    #[test]
    fn token_apis_reject_unsupported_classes_access_and_unmapped_sids() {
        const TOKEN_QUERY: u32 = 0x0008;
        const TOKEN_ADJUST_PRIVILEGES: u32 = 0x0020;
        let mut process = token_test_process();
        let output = BASE + 0x100;
        process.wr32(output, 0xdead_beef).unwrap();
        set_registry_x86_args(
            &mut process,
            &[u32::MAX, TOKEN_ADJUST_PRIVILEGES, output as u32],
        );
        assert_eq!(lookup("OpenProcessToken").unwrap()(&mut process), Ok(0));
        assert_eq!(process.rd32(output), Ok(0));
        assert_eq!(process.rd32(TEB_ADDR + 0x34), Ok(50)); // unsupported token right

        process.extra.advapi32.process_identity = Some(super::TokenIdentity {
            sid: vec![1, 1, 0, 0, 0, 0, 0, 5, 42, 0, 0, 0],
            account_name: "FixtureUser".into(),
            domain_name: "FixtureDomain".into(),
            sid_name_use: 1,
        });
        set_registry_x86_args(&mut process, &[u32::MAX, TOKEN_QUERY, output as u32]);
        assert_eq!(lookup("OpenProcessToken").unwrap()(&mut process), Ok(1));
        let token = process.rd32(output).unwrap();
        let info = BASE + 0x200;
        let returned = BASE + 0x180;
        set_registry_x86_args(&mut process, &[token, 2, info as u32, 32, returned as u32]);
        assert_eq!(lookup("GetTokenInformation").unwrap()(&mut process), Ok(0));
        assert_eq!(process.rd32(TEB_ADDR + 0x34), Ok(50)); // TokenGroups is outside this batch

        let unknown_sid = BASE + 0x300;
        process
            .write_bytes(unknown_sid, &[1, 1, 0, 0, 0, 0, 0, 5, 77, 0, 0, 0])
            .unwrap();
        let name_len = BASE + 0x340;
        let domain_len = BASE + 0x344;
        let use_out = BASE + 0x348;
        process.wr32(name_len, 24).unwrap();
        process.wr32(domain_len, 24).unwrap();
        set_registry_x86_args(
            &mut process,
            &[
                0,
                unknown_sid as u32,
                BASE as u32,
                name_len as u32,
                (BASE + 0x100) as u32,
                domain_len as u32,
                use_out as u32,
            ],
        );
        assert_eq!(lookup("LookupAccountSidW").unwrap()(&mut process), Ok(0));
        assert_eq!(process.rd32(TEB_ADDR + 0x34), Ok(1332)); // ERROR_NONE_MAPPED

        for (name, cleanup) in [
            ("OpenProcessToken", 12),
            ("OpenThreadToken", 16),
            ("DuplicateToken", 12),
            ("GetTokenInformation", 20),
            ("ImpersonateLoggedOnUser", 4),
            ("RevertToSelf", 0),
            ("LookupAccountSidW", 28),
        ] {
            assert!(lookup(name).is_some(), "{name} must resolve");
            assert_eq!(
                crate::x86_abi_generated::cleanup_bytes("advapi32", name),
                Some(cleanup),
                "{name} must have generated x86 cleanup"
            );
        }
    }

    fn registry_x86_key(process: &mut Process, path: &str) -> u32 {
        let key = process.new_handle(Handle::Other) as u32;
        let normalized = path.to_lowercase();
        process.registry_paths.insert(normalized.clone());
        process.registry_key_paths.insert(key as u64, normalized);
        process
            .registry_key_access
            .insert(key as u64, super::KEY_ALL_ACCESS);
        key
    }

    #[test]
    fn reg_open_key_ex_a_returns_a_handle_with_requested_access_for_empty_subkey() {
        const HKEY_CURRENT_USER: u64 = 0xffff_ffff_8000_0001;
        let (mut process, result) = registry_process(HKEY_CURRENT_USER, None);
        let open = lookup("RegOpenKeyExA").expect("RegOpenKeyExA should be implemented");

        assert_eq!(open(&mut process), Ok(0));
        let key = process.rd64(result).unwrap();
        assert_ne!(key, HKEY_CURRENT_USER);
        assert_eq!(
            process.registry_key_paths.get(&key).unwrap(),
            "hkey_current_user"
        );
        assert_eq!(process.registry_key_access.get(&key), Some(&0x20019));
    }

    #[test]
    fn reg_create_key_a_builds_nested_guest_registry_paths_and_returns_handle() {
        const HKEY_CURRENT_USER: u32 = 0x8000_0001;
        let mut process = registry_x86_call(&[]);
        let subkey = BASE + 0x100;
        let output = BASE + 0x180;
        process
            .write_cstr(subkey, "Software\\AnyWASM\\Settings")
            .unwrap();
        set_registry_x86_args(
            &mut process,
            &[HKEY_CURRENT_USER, subkey as u32, output as u32],
        );

        assert_eq!(lookup("RegCreateKeyA").unwrap()(&mut process), Ok(0));
        let key = process.rd32(output).unwrap() as u64;
        assert_ne!(key, 0);
        assert_eq!(
            process.registry_key_paths.get(&key).unwrap(),
            "hkey_current_user\\software\\anywasm\\settings"
        );
        assert!(process
            .registry_children
            .get("hkey_current_user\\software\\anywasm")
            .unwrap()
            .iter()
            .any(|name| name == "Settings"));
    }

    #[test]
    fn reg_set_value_ex_w_stores_exact_utf16_value_bytes_for_wide_queries() {
        let mut process = registry_x86_call(&[]);
        let path = "hkey_current_user\\software\\wide";
        let key = registry_x86_key(&mut process, path);
        let value_name = BASE + 0x100;
        let value_data = BASE + 0x180;
        let output = BASE + 0x240;
        let size_ptr = BASE + 0x280;
        let encoded = "D:\\The Sims 3\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        process.write_wstr(value_name, "install dir").unwrap();
        process.write_bytes(value_data, &encoded).unwrap();
        set_registry_x86_args(
            &mut process,
            &[
                key,
                value_name as u32,
                0,
                1,
                value_data as u32,
                encoded.len() as u32,
            ],
        );

        assert_eq!(lookup("RegSetValueExW").unwrap()(&mut process), Ok(0));
        assert_eq!(
            process
                .registry_values
                .get(&(path.to_owned(), "install dir".to_owned())),
            Some(&(1, encoded.clone()))
        );

        set_registry_x86_args(
            &mut process,
            &[key, value_name as u32, 0, 0, output as u32, size_ptr as u32],
        );
        process.wr32(size_ptr, encoded.len() as u32).unwrap();
        assert_eq!(lookup("RegQueryValueExW").unwrap()(&mut process), Ok(0));
        assert_eq!(process.read_bytes(output, encoded.len()).unwrap(), encoded);
    }

    #[test]
    fn reg_open_key_ex_a_reports_missing_subkey_without_changing_output() {
        const HKEY_CURRENT_USER: u64 = 0xffff_ffff_8000_0001;
        let (mut process, result) = registry_process(HKEY_CURRENT_USER, Some("Software\\AnyWASM"));
        let open = lookup("RegOpenKeyExA").expect("RegOpenKeyExA should be implemented");

        assert!(matches!(
            open(&mut process),
            Err(Stop::Unimplemented { dll, .. }) if dll == "advapi32"
        ));
        assert_eq!(process.rd64(result), Ok(0xfeed_face_cafe_beef));
    }

    #[test]
    fn registry_handles_enforce_the_access_mask_requested_when_opened() {
        const HKEY_CURRENT_USER: u32 = 0x8000_0001;
        const KEY_QUERY_VALUE: u32 = 0x0001;
        const KEY_SET_VALUE: u32 = 0x0002;
        const ERROR_ACCESS_DENIED: u64 = 5;
        const BASE_PATH: &str = "hkey_current_user\\software\\rights";
        let subkey = BASE + 0x100;
        let output = BASE + 0x180;
        let value_name = BASE + 0x200;
        let data = BASE + 0x280;
        let mut process = registry_x86_call(&[]);
        process.write_cstr(subkey, "Software\\Rights").unwrap();
        process.write_cstr(value_name, "mode").unwrap();
        process.write_cstr(data, "changed").unwrap();
        process.registry_paths.insert(BASE_PATH.to_owned());

        let open = lookup("RegOpenKeyExA").unwrap();
        set_registry_x86_args(
            &mut process,
            &[
                HKEY_CURRENT_USER,
                subkey as u32,
                0,
                KEY_QUERY_VALUE,
                output as u32,
            ],
        );
        assert_eq!(open(&mut process), Ok(0));
        let key = process.rd32(output).unwrap() as u64;

        let set = lookup("RegSetValueExA").unwrap();
        set_registry_x86_args(
            &mut process,
            &[key as u32, value_name as u32, 0, 1, data as u32, 8],
        );
        assert_eq!(set(&mut process), Ok(ERROR_ACCESS_DENIED));
        assert!(!process
            .registry_values
            .contains_key(&(BASE_PATH.to_owned(), "mode".to_owned())));

        set_registry_x86_args(
            &mut process,
            &[
                HKEY_CURRENT_USER,
                subkey as u32,
                0,
                KEY_SET_VALUE,
                output as u32,
            ],
        );
        assert_eq!(open(&mut process), Ok(0));
        let key = process.rd32(output).unwrap() as u64;
        let query = lookup("RegQueryValueExA").unwrap();
        set_registry_x86_args(&mut process, &[key as u32, value_name as u32, 0, 0, 0, 0]);
        assert_eq!(query(&mut process), Ok(ERROR_ACCESS_DENIED));
    }

    #[test]
    fn reg_open_key_ex_a_rejects_unknown_root_without_changing_output() {
        let (mut process, result) = registry_process(0x1234, None);
        let open = lookup("RegOpenKeyExA").expect("RegOpenKeyExA should be implemented");

        assert_eq!(open(&mut process), Ok(6)); // ERROR_INVALID_HANDLE
        assert_eq!(process.rd64(result), Ok(0xfeed_face_cafe_beef));
    }

    #[test]
    fn reg_query_info_key_a_reports_counts_and_maximum_lengths() {
        let base = "hkey_local_machine\\software\\sims(steam)";
        let mut process = registry_x86_call(&[]);
        let key = registry_x86_key(&mut process, base);
        process.registry_paths.insert(format!("{base}\\the sims 3"));
        process
            .registry_paths
            .insert(format!("{base}\\expansion pack"));
        process.registry_key_metadata.insert(
            base.to_owned(),
            RegistryKeyMetadata {
                class_name: "SimsRoot".to_owned(),
                security_descriptor_size: Some(120),
                last_write_filetime: Some(0x0123_4567_89ab_cdef),
            },
        );
        process.registry_key_metadata.insert(
            format!("{base}\\the sims 3"),
            RegistryKeyMetadata {
                class_name: "BaseGame".to_owned(),
                security_descriptor_size: Some(96),
                last_write_filetime: Some(0x2345),
            },
        );
        process.registry_key_metadata.insert(
            format!("{base}\\expansion pack"),
            RegistryKeyMetadata {
                class_name: "Expansion".to_owned(),
                security_descriptor_size: Some(88),
                last_write_filetime: Some(0x3456),
            },
        );
        process.registry_children.insert(
            base.to_owned(),
            vec!["The Sims 3".to_owned(), "Expansion Pack".to_owned()],
        );
        process.registry_values.insert(
            (base.to_owned(), "install dir".to_owned()),
            (1, b"D:\\The Sims 3\0".to_vec()),
        );
        process.registry_values.insert(
            (base.to_owned(), "productid".to_owned()),
            (4, 1001u32.to_le_bytes().to_vec()),
        );
        let class = BASE + 0x100;
        let class_len = BASE + 0x180;
        let subkeys = BASE + 0x200;
        let max_subkey = BASE + 0x204;
        let max_class = BASE + 0x208;
        let values = BASE + 0x20c;
        let max_value_name = BASE + 0x210;
        let max_value_data = BASE + 0x214;
        let security_size = BASE + 0x220;
        let filetime = BASE + 0x228;
        process.wr32(class_len, 32).unwrap();
        set_registry_x86_args(
            &mut process,
            &[
                key,
                class as u32,
                class_len as u32,
                0,
                subkeys as u32,
                max_subkey as u32,
                max_class as u32,
                values as u32,
                max_value_name as u32,
                max_value_data as u32,
                security_size as u32,
                filetime as u32,
            ],
        );
        let query = lookup("RegQueryInfoKeyA").expect("RegQueryInfoKeyA should be implemented");

        assert_eq!(query(&mut process), Ok(0));
        assert_eq!(process.read_cstr(class), Ok("SimsRoot".to_owned()));
        assert_eq!(process.rd32(class_len), Ok(8));
        assert_eq!(process.rd32(subkeys), Ok(2));
        assert_eq!(process.rd32(max_subkey), Ok("Expansion Pack".len() as u32));
        assert_eq!(process.rd32(max_class), Ok("Expansion".len() as u32));
        assert_eq!(process.rd32(values), Ok(2));
        assert_eq!(process.rd32(max_value_name), Ok("install dir".len() as u32));
        assert_eq!(process.rd32(max_value_data), Ok(14));
        assert_eq!(process.rd32(security_size), Ok(120));
        assert_eq!(process.rd64(filetime), Ok(0x0123_4567_89ab_cdef));
    }

    #[test]
    fn reg_enum_key_ex_a_returns_name_and_reports_required_capacity() {
        let base = "hkey_local_machine\\software\\sims(steam)";
        let name = BASE + 0x100;
        let name_len = BASE + 0x180;
        let key_class = BASE + 0x200;
        let class_len = BASE + 0x280;
        let filetime = BASE + 0x300;
        let mut process = registry_x86_call(&[]);
        let key = registry_x86_key(&mut process, base);
        process.registry_paths.insert(format!("{base}\\the sims 3"));
        process
            .registry_paths
            .insert(format!("{base}\\expansion pack"));
        process.registry_children.insert(
            base.to_owned(),
            vec!["The Sims 3".to_owned(), "Expansion Pack".to_owned()],
        );
        process.registry_key_metadata.insert(
            format!("{base}\\the sims 3"),
            RegistryKeyMetadata {
                class_name: "Base Game".to_owned(),
                security_descriptor_size: Some(120),
                last_write_filetime: Some(0x0123_4567_89ab_cdef),
            },
        );
        set_registry_x86_args(
            &mut process,
            &[
                key,
                0,
                name as u32,
                name_len as u32,
                0,
                key_class as u32,
                class_len as u32,
                filetime as u32,
            ],
        );
        process.wr32(name_len, 32).unwrap();
        process.wr32(class_len, 32).unwrap();
        let stack = process.cpu.gpr[RSP];
        process.wr32(stack + 4, key).unwrap();
        let enumerate = lookup("RegEnumKeyExA").expect("RegEnumKeyExA should be implemented");

        assert_eq!(enumerate(&mut process), Ok(0));
        assert_eq!(process.read_cstr(name), Ok("The Sims 3".to_owned()));
        assert_eq!(process.rd32(name_len), Ok(10));
        assert_eq!(process.read_cstr(key_class), Ok("Base Game".to_owned()));
        assert_eq!(process.rd32(class_len), Ok(9));
        assert_eq!(process.rd64(filetime), Ok(0x0123_4567_89ab_cdef));

        process.wr32(name_len, 4).unwrap();
        process.write_bytes(name, &[0xcc; 16]).unwrap();
        assert_eq!(enumerate(&mut process), Ok(234)); // ERROR_MORE_DATA
        assert_eq!(process.rd32(name_len), Ok(10));
        assert_eq!(process.read_bytes(name, 4), Ok(vec![0xcc; 4]));
    }

    #[test]
    fn reg_query_value_ex_w_returns_exact_wide_registry_bytes() {
        let mut process = registry_x86_call(&[]);
        let base = "hkey_local_machine\\software\\sims(steam)";
        let key = registry_x86_key(&mut process, base);
        let name = BASE + 0x100;
        let size = BASE + 0x180;
        let value = BASE + 0x200;
        process.write_wstr(name, "install dir").unwrap();
        let data = "D:\\The Sims 3\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        process.registry_values.insert(
            (base.to_owned(), "install dir".to_owned()),
            (1, data.clone()),
        );
        process
            .registry_host_values
            .insert((base.to_owned(), "install dir".to_owned()));
        let mut args = vec![key, name as u32, 0, 0, 0, size as u32];
        set_registry_x86_args(&mut process, &args);
        process.wr32(size, 0).unwrap();
        let query = lookup("RegQueryValueExW").expect("RegQueryValueExW should be implemented");

        assert_eq!(query(&mut process), Ok(0));
        assert_eq!(process.rd32(size), Ok(data.len() as u32));
        args[0] = key;
        args[4] = value as u32;
        set_registry_x86_args(&mut process, &args);
        process.wr32(size, data.len() as u32).unwrap();

        assert_eq!(query(&mut process), Ok(0));
        assert_eq!(process.read_bytes(value, data.len()), Ok(data));
    }

    #[test]
    fn registry_calls_outside_the_supplied_snapshot_stop_as_unimplemented() {
        let subkey = BASE + 0x100;
        let output = BASE + 0x180;
        let mut process = registry_x86_call(&[]);
        process
            .write_cstr(subkey, "Hardware\\Description\\System")
            .unwrap();
        set_registry_x86_args(
            &mut process,
            &[0x8000_0002, subkey as u32, 0, 0x20019, output as u32],
        );
        let open = lookup("RegOpenKeyExA").expect("RegOpenKeyExA should be implemented");

        assert!(matches!(
            open(&mut process),
            Err(Stop::Unimplemented { dll, name }) if dll == "advapi32" && name.contains("outside the supplied read-only snapshot")
        ));
        assert_eq!(process.rd32(output), Ok(0));
    }

    #[test]
    fn crypt_acquire_context_a_creates_a_real_ephemeral_provider_handle() {
        const CRYPT_VERIFYCONTEXT_SILENT: u32 = 0xf000_0040;
        let mut process = registry_x86_call(&[]);
        let provider_name = BASE + 0x100;
        let provider_out = BASE + 0x200;
        process
            .write_cstr(provider_name, "Microsoft Base Cryptographic Provider v1.0")
            .unwrap();
        assert!(process.mem.map(TEB_ADDR, PAGE_SIZE, PROT_RW));
        process.wr32(provider_out, 0xdead_beef).unwrap();
        set_registry_x86_args(
            &mut process,
            &[
                provider_out as u32,
                0,
                provider_name as u32,
                1, // PROV_RSA_FULL
                CRYPT_VERIFYCONTEXT_SILENT,
            ],
        );
        process.set_last_error(0x1234_5678);
        let acquire = lookup("CryptAcquireContextA")
            .expect("CryptAcquireContextA should create a provider context");

        assert_eq!(acquire(&mut process), Ok(1));
        let provider = process.rd32(provider_out).unwrap() as u64;
        assert_ne!(provider, 0);
        assert!(!process.handles.contains_key(&provider));
        assert_eq!(process.rd32(TEB_ADDR + 0x34), Ok(0x1234_5678));
    }

    #[test]
    fn crypt_gen_random_writes_generated_bytes_and_release_closes_the_provider() {
        let mut process = registry_x86_call(&[]);
        let provider = process.new_crypto_provider(CryptoProviderState {
            provider_type: 1,
            provider_name: "Microsoft Base Cryptographic Provider v1.0".to_owned(),
            container: None,
            flags: 0xf000_0040,
        });
        let output = BASE + 0x200;
        set_registry_x86_args(&mut process, &[provider as u32, 4, output as u32]);
        assert_eq!(
            crypt_gen_random_with(&mut process, |bytes| {
                bytes.copy_from_slice(&[0x13, 0x57, 0x9b, 0xdf]);
                true
            }),
            Ok(1)
        );
        assert_eq!(
            process.read_bytes(output, 4),
            Ok(vec![0x13, 0x57, 0x9b, 0xdf])
        );

        set_registry_x86_args(&mut process, &[provider as u32, 0]);
        let release =
            lookup("CryptReleaseContext").expect("CryptReleaseContext should be implemented");
        assert_eq!(release(&mut process), Ok(1));
        assert!(!process.crypto_providers.contains_key(&provider));
    }

    #[test]
    fn crypt_acquire_context_a_exposes_untranslated_persistent_container_semantics() {
        let mut process = registry_x86_call(&[]);
        let provider_name = BASE + 0x100;
        let provider_out = BASE + 0x200;
        process
            .write_cstr(provider_name, "Microsoft Base Cryptographic Provider v1.0")
            .unwrap();
        process.wr32(provider_out, 0xdead_beef).unwrap();
        set_registry_x86_args(
            &mut process,
            &[provider_out as u32, 0, provider_name as u32, 1, 0],
        );
        let acquire = lookup("CryptAcquireContextA").unwrap();
        assert!(matches!(
            acquire(&mut process),
            Err(Stop::Unimplemented { dll, name })
                if dll == "advapi32" && name.contains("persistent key-container semantics")
        ));
        assert_eq!(process.rd32(provider_out), Ok(0xdead_beef));
        assert!(process.crypto_providers.is_empty());
    }

    #[test]
    fn crypt_md5_hash_data_and_get_hash_param_match_the_standard_digest() {
        let mut process = registry_x86_call(&[]);
        let provider = process.new_crypto_provider(CryptoProviderState {
            provider_type: 1,
            provider_name: "Microsoft Base Cryptographic Provider v1.0".to_owned(),
            container: None,
            flags: 0xf000_0040,
        });
        let hash_out = BASE + 0x100;
        let hash_input = BASE + 0x200;
        let digest_out = BASE + 0x300;
        let digest_length = BASE + 0x340;
        process.write_bytes(hash_input, b"abc").unwrap();
        process.wr32(digest_length, 16).unwrap();
        set_registry_x86_args(
            &mut process,
            &[provider as u32, 0x8003, 0, 0, hash_out as u32],
        );
        let create_hash = lookup("CryptCreateHash").expect("CryptCreateHash should be implemented");
        assert_eq!(create_hash(&mut process), Ok(1));
        let hash = process.rd32(hash_out).unwrap();
        assert_ne!(hash, 0);

        set_registry_x86_args(&mut process, &[hash, hash_input as u32, 3, 0]);
        let hash_data = lookup("CryptHashData").expect("CryptHashData should be implemented");
        assert_eq!(hash_data(&mut process), Ok(1));

        set_registry_x86_args(
            &mut process,
            &[hash, 2, digest_out as u32, digest_length as u32, 0],
        );
        let get_hash_param = lookup("CryptGetHashParam")
            .expect("CryptGetHashParam should return the computed digest");
        assert_eq!(get_hash_param(&mut process), Ok(1));
        assert_eq!(process.rd32(digest_length), Ok(16));
        assert_eq!(
            process.read_bytes(digest_out, 16),
            Ok(vec![
                0x90, 0x01, 0x50, 0x98, 0x3c, 0xd2, 0x4f, 0xb0, 0xd6, 0x96, 0x3f, 0x7d, 0x28, 0xe1,
                0x7f, 0x72,
            ])
        );

        set_registry_x86_args(&mut process, &[hash, hash_input as u32, 1, 0]);
        let hash_data = lookup("CryptHashData").unwrap();
        assert_eq!(hash_data(&mut process), Ok(0));

        set_registry_x86_args(&mut process, &[hash]);
        let destroy_hash =
            lookup("CryptDestroyHash").expect("CryptDestroyHash should be implemented");
        assert_eq!(destroy_hash(&mut process), Ok(1));
    }

    #[test]
    fn crypt_derive_key_creates_rc4_key_and_finishes_the_source_hash() {
        let mut process = registry_x86_call(&[]);
        let provider = process.new_crypto_provider(CryptoProviderState {
            provider_type: 1,
            provider_name: "Microsoft Base Cryptographic Provider v1.0".to_owned(),
            container: None,
            flags: 0xf000_0040,
        });
        let hash = process.new_crypto_hash(CryptoHashState {
            provider,
            algorithm: 0x8003,
            data: b"abc".to_vec(),
            finalized: false,
        });
        let key_out = BASE + 0x200;
        set_registry_x86_args(
            &mut process,
            &[provider as u32, 0x6801, hash as u32, 1, key_out as u32],
        );
        let derive = lookup("CryptDeriveKey").expect("CryptDeriveKey should create the RC4 key");

        assert_eq!(derive(&mut process), Ok(1));
        let key = process.rd32(key_out).unwrap();
        assert_ne!(key, 0);
        assert!(process.crypto_hashes[&hash].finalized);
        assert_eq!(
            process.crypto_keys[&(key as u64)].key_material,
            [0x90, 0x01, 0x50, 0x98, 0x3c]
        );
        assert!(process.crypto_keys[&(key as u64)].exportable);
    }

    #[test]
    fn crypt_encrypt_and_decrypt_match_the_rc4_reference_vector() {
        let mut process = registry_x86_call(&[]);
        let key = process.new_crypto_key(CryptoKeyState {
            provider: 0x777,
            algorithm: 0x6801,
            key_material: b"Key".to_vec(),
            exportable: true,
            rc4: Rc4State::new(b"Key"),
        });
        let data = BASE + 0x100;
        let length = BASE + 0x180;
        process.write_bytes(data, b"Plaintext").unwrap();
        process.wr32(length, 9).unwrap();
        set_registry_x86_args(
            &mut process,
            &[key as u32, 0, 1, 0, data as u32, length as u32, 9],
        );
        let encrypt = lookup("CryptEncrypt").expect("CryptEncrypt should translate RC4");
        assert_eq!(encrypt(&mut process), Ok(1));
        assert_eq!(
            process.read_bytes(data, 9),
            Ok(vec![0xbb, 0xf3, 0x16, 0xe8, 0xd9, 0x40, 0xaf, 0x0a, 0xd3])
        );

        let decrypt_key = process.new_crypto_key(CryptoKeyState {
            provider: 0x777,
            algorithm: 0x6801,
            key_material: b"Key".to_vec(),
            exportable: true,
            rc4: Rc4State::new(b"Key"),
        });
        set_registry_x86_args(
            &mut process,
            &[decrypt_key as u32, 0, 1, 0, data as u32, length as u32],
        );
        let decrypt = lookup("CryptDecrypt").expect("CryptDecrypt should translate RC4");
        assert_eq!(decrypt(&mut process), Ok(1));
        assert_eq!(process.read_bytes(data, 9), Ok(b"Plaintext".to_vec()));
    }
}
