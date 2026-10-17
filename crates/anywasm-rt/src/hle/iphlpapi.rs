//! Deterministic IP Helper adapter snapshot for the virtual Winsock loopback network.
//!
//! The runtime exposes one synthetic IPv4 software loopback adapter.  The returned records
//! live entirely in the guest-provided buffer and contain no host network information.

use crate::process::{HleFn, Process, Stop};
use anywasm_cpu::CpuMode;

const ERROR_SUCCESS: u32 = 0;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_BUFFER_OVERFLOW: u32 = 111;
const ERROR_NO_DATA: u32 = 232;

const AF_UNSPEC: u32 = 0;
const AF_INET: u32 = 2;
const AF_INET6: u32 = 23;

const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
const IF_OPER_STATUS_UP: u32 = 1;
const IP_ADAPTER_IPV4_ENABLED: u32 = 0x80;

const GAA_FLAG_SKIP_UNICAST: u32 = 0x0001;
const GAA_FLAG_INCLUDE_PREFIX: u32 = 0x0010;
const GAA_FLAG_SKIP_FRIENDLY_NAME: u32 = 0x0020;
const GAA_FLAG_INCLUDE_ALL_COMPARTMENTS: u32 = 0x0200;
const GAA_FLAG_SKIP_DNS_INFO: u32 = 0x0800;
const GAA_KNOWN_FLAGS: u32 = 0x0fff;

const ADAPTER_NAME: &[u8] = b"ANYWASM_LOOPBACK\0";
const ADAPTER_DESCRIPTION: &str = "AnyWASM software loopback adapter";
const ADAPTER_FRIENDLY_NAME: &str = "AnyWASM Loopback";

pub fn lookup(name: &str) -> Option<HleFn> {
    match name {
        "GetAdaptersInfo" => Some(get_adapters_info),
        "GetAdaptersAddresses" => Some(get_adapters_addresses),
        _ => None,
    }
}

fn pointer_size(p: &Process) -> usize {
    if p.cpu.mode == CpuMode::X86_32 {
        4
    } else {
        8
    }
}

fn align_up(value: usize, alignment: usize) -> usize {
    (value + alignment - 1) & !(alignment - 1)
}

#[derive(Default)]
struct LayoutBuilder {
    size: usize,
    alignment: usize,
}

impl LayoutBuilder {
    fn field(&mut self, size: usize, alignment: usize) -> usize {
        self.size = align_up(self.size, alignment);
        let offset = self.size;
        self.size += size;
        self.alignment = self.alignment.max(alignment);
        offset
    }

    fn finish(self) -> usize {
        align_up(self.size, self.alignment.max(1))
    }
}

#[derive(Debug)]
struct IpAddrStringLayout {
    size: usize,
    ip: usize,
    mask: usize,
}

fn ip_addr_string_layout(ptr: usize) -> IpAddrStringLayout {
    let mut b = LayoutBuilder::default();
    b.field(ptr, ptr);
    let ip = b.field(16, 1);
    let mask = b.field(16, 1);
    b.field(4, 4);
    IpAddrStringLayout {
        size: b.finish(),
        ip,
        mask,
    }
}

#[derive(Debug)]
struct AdapterInfoLayout {
    size: usize,
    combo_index: usize,
    name: usize,
    description: usize,
    address_length: usize,
    address: usize,
    index: usize,
    kind: usize,
    dhcp_enabled: usize,
    ip_list: usize,
    gateway_list: usize,
    dhcp_server: usize,
    have_wins: usize,
    ip_string: IpAddrStringLayout,
}

fn adapter_info_layout(ptr: usize) -> AdapterInfoLayout {
    let mut b = LayoutBuilder::default();
    b.field(ptr, ptr); // Next
    let combo_index = b.field(4, 4);
    let name = b.field(260, 1);
    let description = b.field(132, 1);
    let address_length = b.field(4, 4);
    let address = b.field(8, 1);
    let index = b.field(4, 4);
    let kind = b.field(4, 4);
    let dhcp_enabled = b.field(4, 4);
    b.field(ptr, ptr); // CurrentIpAddress, reserved and NULL
    let ip_string = ip_addr_string_layout(ptr);
    let ip_list = b.field(ip_string.size, ptr.max(4));
    let gateway_list = b.field(ip_string.size, ptr.max(4));
    let dhcp_server = b.field(ip_string.size, ptr.max(4));
    let have_wins = b.field(4, 4);
    b.field(ip_string.size, ptr.max(4)); // PrimaryWinsServer
    b.field(ip_string.size, ptr.max(4)); // SecondaryWinsServer
                                         // VS 2005 and later use 64-bit time_t by default, including x86.
    b.field(8, 8); // LeaseObtained
    b.field(8, 8); // LeaseExpires
    AdapterInfoLayout {
        size: b.finish(),
        combo_index,
        name,
        description,
        address_length,
        address,
        index,
        kind,
        dhcp_enabled,
        ip_list,
        gateway_list,
        dhcp_server,
        have_wins,
        ip_string,
    }
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn put_ptr(bytes: &mut [u8], offset: usize, ptr_size: usize, value: u64) {
    if ptr_size == 4 {
        put_u32(bytes, offset, value as u32);
    } else {
        put_u64(bytes, offset, value);
    }
}

fn put_fixed_string(bytes: &mut [u8], offset: usize, capacity: usize, value: &[u8]) {
    let count = value.len().min(capacity.saturating_sub(1));
    bytes[offset..offset + count].copy_from_slice(&value[..count]);
}

fn put_ip_addr_string(
    bytes: &mut [u8],
    offset: usize,
    layout: &IpAddrStringLayout,
    address: &[u8],
    mask: &[u8],
) {
    put_fixed_string(bytes, offset + layout.ip, 16, address);
    put_fixed_string(bytes, offset + layout.mask, 16, mask);
}

fn valid_guest_range(p: &Process, address: u64, len: usize) -> bool {
    if address == 0 || address.checked_add(len as u64).is_none() {
        return false;
    }
    p.mem.read_vec(address, len).is_ok()
}

fn write_guest(p: &mut Process, address: u64, bytes: &[u8]) -> bool {
    p.mem.write_from(address, bytes).is_ok()
}

fn read_u32(p: &Process, address: u64) -> Option<u32> {
    p.mem.read_u32(address).ok()
}

fn write_u32_guest(p: &mut Process, address: u64, value: u32) -> bool {
    p.mem.write_u32(address, value).is_ok()
}

fn guest_pointer(p: &Process, base: u64, offset: usize) -> Option<u64> {
    let address = base.checked_add(offset as u64)?;
    if pointer_size(p) == 4 && address > u32::MAX as u64 {
        None
    } else {
        Some(address)
    }
}

fn get_adapters_info(p: &mut Process) -> Result<u64, Stop> {
    let adapter_info = p.arg(0);
    let size_pointer = p.arg(1);
    if size_pointer == 0 {
        return Ok(ERROR_INVALID_PARAMETER as u64);
    }
    let Some(output_capacity) = read_u32(p, size_pointer) else {
        return Ok(ERROR_INVALID_PARAMETER as u64);
    };

    let ptr = pointer_size(p);
    let layout = adapter_info_layout(ptr);
    let Ok(required) = u32::try_from(layout.size) else {
        return Ok(ERROR_INVALID_PARAMETER as u64);
    };
    if adapter_info == 0 || output_capacity < required {
        if !write_u32_guest(p, size_pointer, required) {
            return Ok(ERROR_INVALID_PARAMETER as u64);
        }
        return Ok(ERROR_BUFFER_OVERFLOW as u64);
    }
    if !valid_guest_range(p, adapter_info, layout.size) {
        return Ok(ERROR_INVALID_PARAMETER as u64);
    }

    let mut bytes = vec![0; layout.size];
    put_u32(&mut bytes, layout.combo_index, 1);
    put_fixed_string(&mut bytes, layout.name, 260, ADAPTER_NAME);
    put_fixed_string(
        &mut bytes,
        layout.description,
        132,
        ADAPTER_DESCRIPTION.as_bytes(),
    );
    // A software loopback adapter has no hardware address and is not DHCP-configured.
    bytes[layout.address..layout.address + 8].fill(0);
    put_u32(&mut bytes, layout.address_length, 0);
    put_u32(&mut bytes, layout.index, 1);
    put_u32(&mut bytes, layout.kind, IF_TYPE_SOFTWARE_LOOPBACK);
    put_u32(&mut bytes, layout.dhcp_enabled, 0);
    put_ip_addr_string(
        &mut bytes,
        layout.ip_list,
        &layout.ip_string,
        b"127.0.0.1",
        b"255.0.0.0",
    );
    put_ip_addr_string(
        &mut bytes,
        layout.gateway_list,
        &layout.ip_string,
        b"0.0.0.0",
        b"0.0.0.0",
    );
    put_ip_addr_string(
        &mut bytes,
        layout.dhcp_server,
        &layout.ip_string,
        b"0.0.0.0",
        b"0.0.0.0",
    );
    put_u32(&mut bytes, layout.have_wins, 0);

    if write_guest(p, adapter_info, &bytes) {
        Ok(ERROR_SUCCESS as u64)
    } else {
        Ok(ERROR_INVALID_PARAMETER as u64)
    }
}

#[derive(Debug)]
struct AdapterAddressesLayout {
    size: usize,
    next: usize,
    adapter_name: usize,
    unicast: usize,
    dns_suffix: usize,
    description: usize,
    friendly_name: usize,
    physical_address: usize,
    physical_address_length: usize,
    flags: usize,
    mtu: usize,
    if_type: usize,
    oper_status: usize,
    ipv6_if_index: usize,
    zone_indices: usize,
    prefix: usize,
}

fn adapter_addresses_layout(ptr: usize) -> AdapterAddressesLayout {
    let mut b = LayoutBuilder::default();
    b.field(8, 8); // Alignment / Length + IfIndex union
    let next = b.field(ptr, ptr);
    let adapter_name = b.field(ptr, ptr);
    let unicast = b.field(ptr, ptr);
    b.field(ptr, ptr); // FirstAnycastAddress
    b.field(ptr, ptr); // FirstMulticastAddress
    b.field(ptr, ptr); // FirstDnsServerAddress
    let dns_suffix = b.field(ptr, ptr);
    let description = b.field(ptr, ptr);
    let friendly_name = b.field(ptr, ptr);
    let physical_address = b.field(8, 1);
    let physical_address_length = b.field(4, 4);
    let flags = b.field(4, 4);
    let mtu = b.field(4, 4);
    let if_type = b.field(4, 4);
    let oper_status = b.field(4, 4);
    let ipv6_if_index = b.field(4, 4);
    let zone_indices = b.field(16 * 4, 4);
    let prefix = b.field(ptr, ptr);
    AdapterAddressesLayout {
        size: align_up(b.size, 8),
        next,
        adapter_name,
        unicast,
        dns_suffix,
        description,
        friendly_name,
        physical_address,
        physical_address_length,
        flags,
        mtu,
        if_type,
        oper_status,
        ipv6_if_index,
        zone_indices,
        prefix,
    }
}

fn unicast_xp_layout(ptr: usize) -> (usize, usize, usize, usize) {
    // IP_ADAPTER_UNICAST_ADDRESS_XP: 8-byte alignment/length union, Next, SOCKET_ADDRESS,
    // PrefixOrigin, SuffixOrigin, DadState and three lifetime fields.
    let mut b = LayoutBuilder::default();
    b.field(8, 8);
    let next = b.field(ptr, ptr);
    let address = b.field(ptr, ptr);
    let address_length = b.field(4, 4);
    b.field(6 * 4, 4);
    (align_up(b.size, 8), next, address, address_length)
}

fn prefix_xp_layout(ptr: usize) -> (usize, usize, usize, usize) {
    // IP_ADAPTER_PREFIX_XP uses the same aligned { Length, Flags } union and SOCKET_ADDRESS.
    let mut b = LayoutBuilder::default();
    b.field(8, 8);
    let next = b.field(ptr, ptr);
    let address = b.field(ptr, ptr);
    let address_length = b.field(4, 4);
    b.field(4, 4); // PrefixLength
    (align_up(b.size, 8), next, address, address_length)
}

#[derive(Debug)]
struct AddressesBufferLayout {
    required: usize,
    root: AdapterAddressesLayout,
    unicast: Option<(usize, usize, usize, usize)>,
    prefix: Option<(usize, usize, usize, usize)>,
    sockaddr: Option<usize>,
    adapter_name: usize,
    description: usize,
    friendly_name: Option<usize>,
}

fn addresses_buffer_layout(ptr: usize, flags: u32) -> AddressesBufferLayout {
    let root = adapter_addresses_layout(ptr);
    let mut size = root.size;
    let unicast = if flags & GAA_FLAG_SKIP_UNICAST == 0 {
        let fields = unicast_xp_layout(ptr);
        size = align_up(size, 8);
        let offset = size;
        size += fields.0;
        Some((offset, fields.1, fields.2, fields.3))
    } else {
        None
    };
    let prefix = if flags & GAA_FLAG_INCLUDE_PREFIX != 0 {
        let fields = prefix_xp_layout(ptr);
        size = align_up(size, 8);
        let offset = size;
        size += fields.0;
        Some((offset, fields.1, fields.2, fields.3))
    } else {
        None
    };
    let sockaddr = if unicast.is_some() || prefix.is_some() {
        size = align_up(size, 4);
        let offset = size;
        size += 16;
        Some(offset)
    } else {
        None
    };
    size = align_up(size, 1);
    let adapter_name = size;
    size += ADAPTER_NAME.len();
    size = align_up(size, 2);
    let description = size;
    size += (ADAPTER_DESCRIPTION.encode_utf16().count() + 1) * 2;
    let friendly_name = if flags & GAA_FLAG_SKIP_FRIENDLY_NAME == 0 {
        size = align_up(size, 2);
        let offset = size;
        size += (ADAPTER_FRIENDLY_NAME.encode_utf16().count() + 1) * 2;
        Some(offset)
    } else {
        None
    };
    AddressesBufferLayout {
        required: size,
        root,
        unicast,
        prefix,
        sockaddr,
        adapter_name,
        description,
        friendly_name,
    }
}

fn write_utf16(bytes: &mut [u8], offset: usize, value: &str) {
    for (index, unit) in value.encode_utf16().chain(std::iter::once(0)).enumerate() {
        let at = offset + index * 2;
        bytes[at..at + 2].copy_from_slice(&unit.to_le_bytes());
    }
}

fn write_sockaddr_in(bytes: &mut [u8], offset: usize) {
    // sockaddr_in is 16 bytes: AF_INET, port 0, 127.0.0.1, and eight zero bytes.
    bytes[offset..offset + 2].copy_from_slice(&(AF_INET as u16).to_le_bytes());
    bytes[offset + 2..offset + 4].copy_from_slice(&0u16.to_le_bytes());
    bytes[offset + 4..offset + 8].copy_from_slice(&[127, 0, 0, 1]);
}

fn put_checked_ptr(
    p: &Process,
    bytes: &mut [u8],
    structure: usize,
    field: usize,
    output: u64,
    target: Option<usize>,
) -> bool {
    let Some(target) = target else {
        put_ptr(bytes, structure + field, pointer_size(p), 0);
        return true;
    };
    let Some(address) = guest_pointer(p, output, target) else {
        return false;
    };
    put_ptr(bytes, structure + field, pointer_size(p), address);
    true
}

fn get_adapters_addresses(p: &mut Process) -> Result<u64, Stop> {
    let family = p.arg(0) as u32;
    let flags = p.arg(1) as u32;
    let reserved = p.arg(2);
    let output = p.arg(3);
    let size_pointer = p.arg(4);
    if size_pointer == 0 || reserved != 0 {
        return Ok(ERROR_INVALID_PARAMETER as u64);
    }
    let Some(output_capacity) = read_u32(p, size_pointer) else {
        return Ok(ERROR_INVALID_PARAMETER as u64);
    };
    if !matches!(family, AF_UNSPEC | AF_INET | AF_INET6)
        || flags & !GAA_KNOWN_FLAGS != 0
        || flags & GAA_FLAG_INCLUDE_ALL_COMPARTMENTS != 0
    {
        return Ok(ERROR_INVALID_PARAMETER as u64);
    }
    if family == AF_INET6 {
        return Ok(ERROR_NO_DATA as u64);
    }

    let ptr = pointer_size(p);
    let layout = addresses_buffer_layout(ptr, flags);
    let Ok(required) = u32::try_from(layout.required) else {
        return Ok(ERROR_INVALID_PARAMETER as u64);
    };
    if output == 0 || output_capacity < required {
        if !write_u32_guest(p, size_pointer, required) {
            return Ok(ERROR_INVALID_PARAMETER as u64);
        }
        return Ok(ERROR_BUFFER_OVERFLOW as u64);
    }
    if !valid_guest_range(p, output, layout.required) {
        return Ok(ERROR_INVALID_PARAMETER as u64);
    }

    let mut bytes = vec![0; layout.required];
    put_u32(&mut bytes, 0, layout.root.size as u32); // XP-compatible Length.
    put_u32(&mut bytes, 4, 1); // IPv4 IfIndex.
    if !put_checked_ptr(p, &mut bytes, 0, layout.root.next, output, None)
        || !put_checked_ptr(
            p,
            &mut bytes,
            0,
            layout.root.adapter_name,
            output,
            Some(layout.adapter_name),
        )
        || !put_checked_ptr(
            p,
            &mut bytes,
            0,
            layout.root.unicast,
            output,
            layout.unicast.map(|value| value.0),
        )
        || !put_checked_ptr(p, &mut bytes, 0, layout.root.dns_suffix, output, None)
        || !put_checked_ptr(
            p,
            &mut bytes,
            0,
            layout.root.description,
            output,
            Some(layout.description),
        )
        || !put_checked_ptr(
            p,
            &mut bytes,
            0,
            layout.root.friendly_name,
            output,
            layout.friendly_name,
        )
        || !put_checked_ptr(
            p,
            &mut bytes,
            0,
            layout.root.prefix,
            output,
            layout.prefix.map(|value| value.0),
        )
    {
        return Ok(ERROR_INVALID_PARAMETER as u64);
    }
    put_u32(&mut bytes, layout.root.physical_address_length, 0);
    bytes[layout.root.physical_address..layout.root.physical_address + 8].fill(0);
    bytes[layout.root.zone_indices..layout.root.zone_indices + 16 * 4].fill(0);
    put_u32(&mut bytes, layout.root.flags, IP_ADAPTER_IPV4_ENABLED);
    put_u32(&mut bytes, layout.root.mtu, 65_536);
    put_u32(&mut bytes, layout.root.if_type, IF_TYPE_SOFTWARE_LOOPBACK);
    put_u32(&mut bytes, layout.root.oper_status, IF_OPER_STATUS_UP);
    put_u32(&mut bytes, layout.root.ipv6_if_index, 0);

    if let (Some((offset, next_field, address_field, address_len_field)), Some(sockaddr)) =
        (layout.unicast, layout.sockaddr)
    {
        let (unicast_size, _, _, _) = unicast_xp_layout(ptr);
        put_u32(&mut bytes, offset, unicast_size as u32);
        let Some(address) = guest_pointer(p, output, sockaddr) else {
            return Ok(ERROR_INVALID_PARAMETER as u64);
        };
        put_ptr(&mut bytes, offset + next_field, ptr, 0);
        put_ptr(&mut bytes, offset + address_field, ptr, address);
        put_u32(&mut bytes, offset + address_len_field, 16);
        put_u32(&mut bytes, offset + address_len_field + 4, 2); // IpPrefixOriginWellKnown
        put_u32(&mut bytes, offset + address_len_field + 8, 2); // IpSuffixOriginWellKnown
        put_u32(&mut bytes, offset + address_len_field + 12, 4); // IpDadStatePreferred
        put_u32(&mut bytes, offset + address_len_field + 16, u32::MAX);
        put_u32(&mut bytes, offset + address_len_field + 20, u32::MAX);
        put_u32(&mut bytes, offset + address_len_field + 24, u32::MAX);
    }
    if let (Some((offset, next_field, address_field, address_len_field)), Some(sockaddr)) =
        (layout.prefix, layout.sockaddr)
    {
        let (prefix_size, _, _, _) = prefix_xp_layout(ptr);
        put_u32(&mut bytes, offset, prefix_size as u32);
        let Some(address) = guest_pointer(p, output, sockaddr) else {
            return Ok(ERROR_INVALID_PARAMETER as u64);
        };
        put_ptr(&mut bytes, offset + next_field, ptr, 0);
        put_ptr(&mut bytes, offset + address_field, ptr, address);
        put_u32(&mut bytes, offset + address_len_field, 16);
        put_u32(&mut bytes, offset + address_len_field + 4, 8);
    }
    if let Some(sockaddr) = layout.sockaddr {
        write_sockaddr_in(&mut bytes, sockaddr);
    }
    bytes[layout.adapter_name..layout.adapter_name + ADAPTER_NAME.len()]
        .copy_from_slice(ADAPTER_NAME);
    write_utf16(&mut bytes, layout.description, ADAPTER_DESCRIPTION);
    if let Some(friendly_name) = layout.friendly_name {
        write_utf16(&mut bytes, friendly_name, ADAPTER_FRIENDLY_NAME);
    }

    if write_guest(p, output, &bytes) {
        Ok(ERROR_SUCCESS as u64)
    } else {
        Ok(ERROR_INVALID_PARAMETER as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{process::Process, vfs::Vfs};
    use anywasm_cpu::{
        cpu::{R8, R9, RCX, RDX, RSP},
        mem::PROT_RW,
    };

    fn process(mode: CpuMode) -> Process {
        let mut p = Process::new(Vfs::new());
        p.cpu.mode = mode;
        if mode == CpuMode::X86_32 {
            p.vmem_next = 0x0010_0000;
        }
        p
    }

    fn set_args(p: &mut Process, args: &[u64]) {
        let stack = p.alloc_virtual(0x1000, PROT_RW);
        p.cpu.gpr[RSP] = stack;
        if p.cpu.mode == CpuMode::X86_32 {
            p.wr32(stack, 0xfeed_face).unwrap();
            for (index, arg) in args.iter().copied().enumerate() {
                p.wr32(stack + 4 + index as u64 * 4, arg as u32).unwrap();
            }
        } else {
            for (index, reg) in [RCX, RDX, R8, R9].into_iter().enumerate() {
                if let Some(arg) = args.get(index) {
                    p.cpu.gpr[reg] = *arg;
                }
            }
            p.wr64(stack, 0xfeed_face).unwrap();
            if let Some(arg) = args.get(4) {
                // The fifth Win64 argument follows the 32-byte caller shadow area.
                p.wr64(stack + 40, *arg).unwrap();
            }
        }
    }

    fn guest_ptr(p: &Process, address: u64) -> u64 {
        if p.cpu.mode == CpuMode::X86_32 {
            p.rd32(address).unwrap() as u64
        } else {
            p.rd64(address).unwrap()
        }
    }

    fn read_ascii(p: &Process, address: u64, length: usize) -> String {
        let bytes = p.mem.read_vec(address, length).unwrap();
        let end = bytes
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(bytes.len());
        String::from_utf8(bytes[..end].to_vec()).unwrap()
    }

    fn read_utf16(p: &Process, address: u64) -> String {
        let mut units = Vec::new();
        for index in 0..128u64 {
            let unit = p.rd16(address + index * 2).unwrap();
            if unit == 0 {
                break;
            }
            units.push(unit);
        }
        String::from_utf16(&units).unwrap()
    }

    #[test]
    fn adapter_info_layout_is_exact_for_x86_and_x64_default_time_t() {
        let x86 = adapter_info_layout(4);
        let x64 = adapter_info_layout(8);
        assert_eq!(x86.size, 648);
        assert_eq!(x64.size, 704);
        assert_eq!(x86.ip_string.size, 40);
        assert_eq!(x64.ip_string.size, 48);
    }

    #[test]
    fn get_adapters_info_retries_then_returns_loopback_fields_and_empty_gateway() {
        let mut p = process(CpuMode::X86_32);
        let info = p.alloc_virtual(0x1000, PROT_RW);
        let size = p.alloc_virtual(0x1000, PROT_RW);
        p.wr32(size, 0).unwrap();
        set_args(&mut p, &[info, size]);
        let get = lookup("GetAdaptersInfo").unwrap();
        assert_eq!(get(&mut p).unwrap(), ERROR_BUFFER_OVERFLOW as u64);
        assert_eq!(p.rd32(size).unwrap(), 648);

        p.wr32(size, 648).unwrap();
        set_args(&mut p, &[info, size]);
        assert_eq!(get(&mut p).unwrap(), ERROR_SUCCESS as u64);
        let layout = adapter_info_layout(4);
        assert_eq!(
            read_ascii(&p, info + layout.name as u64, 260),
            "ANYWASM_LOOPBACK"
        );
        assert_eq!(p.rd32(info + layout.index as u64).unwrap(), 1);
        assert_eq!(
            p.rd32(info + layout.kind as u64).unwrap(),
            IF_TYPE_SOFTWARE_LOOPBACK
        );
        assert_eq!(p.rd32(info + layout.address_length as u64).unwrap(), 0);
        assert_eq!(
            p.mem.read_vec(info + layout.address as u64, 8).unwrap(),
            [0; 8]
        );
        assert_eq!(p.rd32(info + layout.dhcp_enabled as u64).unwrap(), 0);
        assert_eq!(
            read_ascii(
                &p,
                info + layout.ip_list as u64 + layout.ip_string.ip as u64,
                16
            ),
            "127.0.0.1"
        );
        assert_eq!(
            read_ascii(
                &p,
                info + layout.ip_list as u64 + layout.ip_string.mask as u64,
                16
            ),
            "255.0.0.0"
        );
        assert_eq!(
            read_ascii(
                &p,
                info + layout.gateway_list as u64 + layout.ip_string.ip as u64,
                16
            ),
            "0.0.0.0"
        );
    }

    #[test]
    fn get_adapters_info_rejects_missing_or_unwritable_length_pointer() {
        let mut p = process(CpuMode::X86_32);
        let info = p.alloc_virtual(0x1000, PROT_RW);
        let get = lookup("GetAdaptersInfo").unwrap();
        set_args(&mut p, &[info, 0]);
        assert_eq!(get(&mut p).unwrap(), ERROR_INVALID_PARAMETER as u64);
        set_args(&mut p, &[info, 0xdead_0000]);
        assert_eq!(get(&mut p).unwrap(), ERROR_INVALID_PARAMETER as u64);
    }

    #[test]
    fn adapter_addresses_xp_layouts_match_x86_and_x64_sdk_prefixes() {
        assert_eq!(adapter_addresses_layout(4).size, 144);
        assert_eq!(adapter_addresses_layout(8).size, 184);
        assert_eq!(unicast_xp_layout(4).0, 48);
        assert_eq!(unicast_xp_layout(8).0, 56);
        assert_eq!(prefix_xp_layout(4).0, 24);
        assert_eq!(prefix_xp_layout(8).0, 32);
    }

    #[test]
    fn get_adapters_addresses_retries_and_links_unicast_and_prefix_records() {
        let mut p = process(CpuMode::X86_32);
        let size = p.alloc_virtual(0x1000, PROT_RW);
        p.wr32(size, 0).unwrap();
        set_args(
            &mut p,
            &[AF_INET as u64, GAA_FLAG_INCLUDE_PREFIX as u64, 0, 0, size],
        );
        let get = lookup("GetAdaptersAddresses").unwrap();
        assert_eq!(get(&mut p).unwrap(), ERROR_BUFFER_OVERFLOW as u64);
        let required = p.rd32(size).unwrap();
        let output = p.alloc_virtual(required as u64, PROT_RW);
        set_args(
            &mut p,
            &[
                AF_INET as u64,
                GAA_FLAG_INCLUDE_PREFIX as u64,
                0,
                output,
                size,
            ],
        );
        assert_eq!(get(&mut p).unwrap(), ERROR_SUCCESS as u64);

        let base = adapter_addresses_layout(4);
        let address = addresses_buffer_layout(4, GAA_FLAG_INCLUDE_PREFIX);
        assert_eq!(p.rd32(output).unwrap(), 144);
        assert_eq!(p.rd32(output + 4).unwrap(), 1);
        assert_eq!(guest_ptr(&p, output + base.next as u64), 0);
        assert_eq!(
            read_ascii(
                &p,
                guest_ptr(&p, output + base.adapter_name as u64),
                ADAPTER_NAME.len()
            ),
            "ANYWASM_LOOPBACK"
        );
        assert_eq!(
            p.rd32(output + base.flags as u64).unwrap(),
            IP_ADAPTER_IPV4_ENABLED
        );
        assert_eq!(
            p.rd32(output + base.if_type as u64).unwrap(),
            IF_TYPE_SOFTWARE_LOOPBACK
        );
        assert_eq!(
            p.rd32(output + base.oper_status as u64).unwrap(),
            IF_OPER_STATUS_UP
        );
        assert_eq!(
            p.rd32(output + base.physical_address_length as u64)
                .unwrap(),
            0
        );

        let unicast_offset = address.unicast.unwrap().0 as u64;
        assert_eq!(p.rd32(output + unicast_offset).unwrap(), 48);
        assert_eq!(guest_ptr(&p, output + unicast_offset + 8), 0);
        let sockaddr = guest_ptr(&p, output + unicast_offset + 12);
        assert_eq!(p.rd32(output + unicast_offset + 16).unwrap(), 16);
        assert_eq!(p.rd16(sockaddr).unwrap(), AF_INET as u16);
        assert_eq!(p.mem.read_vec(sockaddr + 4, 4).unwrap(), [127, 0, 0, 1]);

        let prefix_offset = address.prefix.unwrap().0 as u64;
        assert_eq!(p.rd32(output + prefix_offset).unwrap(), 24);
        let prefix_sockaddr = guest_ptr(&p, output + prefix_offset + 12);
        assert_eq!(p.rd32(output + prefix_offset + 16).unwrap(), 16);
        assert_eq!(p.rd32(output + prefix_offset + 20).unwrap(), 8);
        assert_eq!(p.rd16(prefix_sockaddr).unwrap(), AF_INET as u16);
        assert_eq!(
            p.mem.read_vec(prefix_sockaddr + 4, 4).unwrap(),
            [127, 0, 0, 1]
        );
        let description = guest_ptr(&p, output + base.description as u64);
        assert_eq!(read_utf16(&p, description), ADAPTER_DESCRIPTION);
    }

    #[test]
    fn adapter_addresses_handles_skipped_fields_and_invalid_family_or_reserved() {
        let mut p = process(CpuMode::X86_64);
        let output = p.alloc_virtual(0x1000, PROT_RW);
        let size = p.alloc_virtual(0x1000, PROT_RW);
        let flags = GAA_FLAG_SKIP_UNICAST | GAA_FLAG_SKIP_FRIENDLY_NAME | GAA_FLAG_SKIP_DNS_INFO;
        let layout = addresses_buffer_layout(8, flags);
        p.wr32(size, layout.required as u32).unwrap();
        set_args(&mut p, &[AF_UNSPEC as u64, flags as u64, 0, output, size]);
        let get = lookup("GetAdaptersAddresses").unwrap();
        assert_eq!(get(&mut p).unwrap(), ERROR_SUCCESS as u64);
        let base = adapter_addresses_layout(8);
        assert_eq!(p.rd32(output).unwrap(), 184);
        assert_eq!(
            p.rd32(output + base.if_type as u64).unwrap(),
            IF_TYPE_SOFTWARE_LOOPBACK
        );
        assert_eq!(
            p.rd32(output + base.oper_status as u64).unwrap(),
            IF_OPER_STATUS_UP
        );
        assert_eq!(guest_ptr(&p, output + base.unicast as u64), 0);
        assert_eq!(guest_ptr(&p, output + base.dns_suffix as u64), 0);
        assert_eq!(guest_ptr(&p, output + base.friendly_name as u64), 0);
        assert_eq!(
            p.mem
                .read_vec(output + base.physical_address as u64, 8)
                .unwrap(),
            [0; 8]
        );
        assert_eq!(
            p.mem
                .read_vec(output + base.zone_indices as u64, 16 * 4)
                .unwrap(),
            [0; 64]
        );

        set_args(&mut p, &[AF_INET6 as u64, 0, 0, output, size]);
        assert_eq!(get(&mut p).unwrap(), ERROR_NO_DATA as u64);
        set_args(&mut p, &[1, 0, 0, output, size]);
        assert_eq!(get(&mut p).unwrap(), ERROR_INVALID_PARAMETER as u64);
        set_args(&mut p, &[AF_INET as u64, 0, 1, output, size]);
        assert_eq!(get(&mut p).unwrap(), ERROR_INVALID_PARAMETER as u64);
    }

    #[test]
    fn adapter_addresses_rejects_invalid_size_pointer_and_reports_short_buffers() {
        let mut p = process(CpuMode::X86_32);
        let output = p.alloc_virtual(0x1000, PROT_RW);
        let size = p.alloc_virtual(0x1000, PROT_RW);
        let get = lookup("GetAdaptersAddresses").unwrap();
        set_args(&mut p, &[AF_INET as u64, 0, 0, output, 0]);
        assert_eq!(get(&mut p).unwrap(), ERROR_INVALID_PARAMETER as u64);
        set_args(&mut p, &[AF_INET as u64, 0, 0, output, 0xdead_0000]);
        assert_eq!(get(&mut p).unwrap(), ERROR_INVALID_PARAMETER as u64);

        p.wr32(size, 1).unwrap();
        set_args(&mut p, &[AF_INET as u64, 0, 0, output, size]);
        assert_eq!(get(&mut p).unwrap(), ERROR_BUFFER_OVERFLOW as u64);
        assert_eq!(
            p.rd32(size).unwrap() as usize,
            addresses_buffer_layout(4, 0).required
        );
    }
}
