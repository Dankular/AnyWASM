//! Process-local `vorbisfile.dll` compatibility backed by a real Vorbis decoder.
//!
//! `ov_open_callbacks` consumes the guest's read callback into a compressed, seekable backing
//! store. `ov_read` then decodes one Vorbis packet at a time with Lewton, preserving the decoded
//! packet remainder per `OggVorbis_File`. This lets the runtime call arbitrary guest callback
//! code without storing a borrowed `&mut Process` inside a decoder object.

use super::*;
use crate::process::{HleFn, Process, Stop};
use anywasm_cpu::CpuMode;
use lewton::header::{read_header_ident, HeaderReadError};
use lewton::inside_ogg::OggStreamReader;
use lewton::VorbisError;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;

const OV_EREAD: i32 = -128;
const OV_EINVAL: i32 = -131;
const OV_ENOTVORBIS: i32 = -132;
const OV_EBADHEADER: i32 = -133;
const OV_EVERSION: i32 = -134;
const OV_EBADPACKET: i32 = -136;
const OV_EBADLINK: i32 = -137;
const OV_ENOSEEK: i32 = -138;

const OV_CALLBACKS_X86_SIZE: usize = 16;
const MAX_COMPRESSED_BYTES: usize = 256 * 1024 * 1024;
const READ_CHUNK_BYTES: u32 = 64 * 1024;
const SEEK_SET: u32 = 0;
const SEEK_CUR: u32 = 1;

#[derive(Clone, Copy, Debug, Default)]
struct GuestCallbacks {
    read: u64,
    seek: u64,
    close: u64,
    tell: u64,
}

#[derive(Clone, Debug)]
struct VorbisLink {
    serial: u32,
    channels: u8,
    sample_rate: u32,
    bitrate_maximum: i32,
    bitrate_nominal: i32,
    bitrate_minimum: i32,
    total_samples: Option<u64>,
}

impl VorbisLink {
    fn from_header(serial: u32, ident: &lewton::header::IdentHeader) -> Self {
        Self {
            serial,
            channels: ident.audio_channels,
            sample_rate: ident.audio_sample_rate,
            bitrate_maximum: ident.bitrate_maximum,
            bitrate_nominal: ident.bitrate_nominal,
            bitrate_minimum: ident.bitrate_minimum,
            total_samples: None,
        }
    }

    fn duration_seconds(&self) -> Option<f64> {
        self.total_samples
            .map(|samples| samples as f64 / self.sample_rate as f64)
    }
}

struct VorbisFile {
    /// Compressed source, retained so `ov_time_seek` can rebuild the decoder exactly.
    compressed: Vec<u8>,
    decoder: OggStreamReader<Cursor<Vec<u8>>>,
    datasource: u64,
    callbacks: GuestCallbacks,
    seekable: bool,
    links: Vec<VorbisLink>,
    /// Interleaved 16-bit PCM from at most one decoded packet.
    pending_pcm: Vec<i16>,
    pending_offset: usize,
    pending_link: usize,
    pending_channels: usize,
    pending_rate: u32,
    position_seconds: f64,
    info_pointers: Vec<u64>,
}

#[derive(Default)]
pub struct VorbisState {
    files: HashMap<u64, VorbisFile>,
}

pub fn lookup(name: &str) -> Option<HleFn> {
    Some(match name {
        "ov_open_callbacks" => ov_open_callbacks,
        "ov_clear" => ov_clear,
        "ov_time_total" => ov_time_total,
        "ov_time_tell" => ov_time_tell,
        "ov_read" => ov_read,
        "ov_info" => ov_info,
        "ov_time_seek" => ov_time_seek,
        _ => return None,
    })
}

fn signed_result(value: i32) -> u64 {
    value as u32 as u64
}

fn map_decoder_error(error: &VorbisError) -> i32 {
    match error {
        VorbisError::BadHeader(HeaderReadError::UnsupportedVorbisVersion) => OV_EVERSION,
        VorbisError::BadHeader(_) => OV_EBADHEADER,
        VorbisError::BadAudio(_) => OV_EBADPACKET,
        VorbisError::OggError(_) => OV_EBADHEADER,
    }
}

fn map_seek_error(_error: &VorbisError) -> i32 {
    OV_EBADLINK
}

fn read_callbacks(p: &Process, address: u64) -> Result<GuestCallbacks, Stop> {
    if address == 0 {
        return Err(Stop::Crash("null ov_callbacks pointer".into()));
    }
    let bytes = p.read_bytes(address, OV_CALLBACKS_X86_SIZE)?;
    let word =
        |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as u64;
    Ok(GuestCallbacks {
        read: word(0),
        seek: word(4),
        close: word(8),
        tell: word(12),
    })
}

/// `fread`-style callbacks return elements, so request byte elements (`size=1`).
fn read_callback_source(
    p: &mut Process,
    datasource: u64,
    callbacks: GuestCallbacks,
    initial: u64,
    initial_bytes: usize,
) -> Result<Result<Vec<u8>, i32>, Stop> {
    if initial_bytes > MAX_COMPRESSED_BYTES {
        return Ok(Err(OV_EREAD));
    }
    if initial_bytes > 0 && initial == 0 {
        return Ok(Err(OV_EINVAL));
    }

    let mut compressed = if initial_bytes > 0 {
        p.read_bytes(initial, initial_bytes)?
    } else {
        Vec::new()
    };
    if callbacks.read == 0 {
        return Ok(if compressed.is_empty() {
            Err(OV_EREAD)
        } else {
            Ok(compressed)
        });
    }

    let scratch = p.alloc_virtual(READ_CHUNK_BYTES as u64, anywasm_cpu::mem::PROT_RW);
    loop {
        let count = p.call_guest(
            callbacks.read,
            &[scratch, 1, READ_CHUNK_BYTES as u64, datasource],
        )? as u32;
        if count == 0 {
            break;
        }
        if count > READ_CHUNK_BYTES
            || compressed.len().saturating_add(count as usize) > MAX_COMPRESSED_BYTES
        {
            return Ok(Err(OV_EREAD));
        }
        compressed.extend_from_slice(&p.read_bytes(scratch, count as usize)?);
    }
    Ok(Ok(compressed))
}

fn source_is_seekable(
    p: &mut Process,
    datasource: u64,
    callbacks: GuestCallbacks,
) -> Result<bool, Stop> {
    if callbacks.seek == 0 || callbacks.tell == 0 {
        return Ok(false);
    }
    let start = p.call_guest(callbacks.tell, &[datasource])? as u32 as i32;
    if start < 0 {
        return Ok(false);
    }
    let restored = p.call_guest(
        callbacks.seek,
        &[datasource, start as u32 as u64, 0, SEEK_SET as u64],
    )? as u32 as i32;
    if restored != 0 {
        return Ok(false);
    }
    // Querying the current position is part of the callback contract; it also avoids treating a
    // non-seekable callback pair as seekable merely because both function pointers are non-null.
    let _ = SEEK_CUR;
    Ok(true)
}

fn parse_ogg_links(bytes: &[u8]) -> Vec<VorbisLink> {
    let mut links = Vec::<VorbisLink>::new();
    let mut link_index_by_serial = HashMap::<u32, usize>::new();
    let mut first_packet_by_serial = HashMap::<u32, Vec<u8>>::new();
    let mut completed_serials = HashSet::<u32>::new();
    let mut page_granules = HashMap::<u32, u64>::new();

    let mut page = 0usize;
    while page.saturating_add(27) <= bytes.len() {
        if &bytes[page..page + 4] != b"OggS" {
            let Some(next) = bytes[page + 1..]
                .windows(4)
                .position(|window| window == b"OggS")
            else {
                break;
            };
            page += next + 1;
            continue;
        }
        if bytes[page + 4] != 0 {
            break;
        }
        let segment_count = bytes[page + 26] as usize;
        let lacing_start = page + 27;
        let payload_start = lacing_start.saturating_add(segment_count);
        if payload_start > bytes.len() {
            break;
        }
        let payload_bytes: usize = bytes[lacing_start..payload_start]
            .iter()
            .map(|segment| *segment as usize)
            .sum();
        let page_end = payload_start.saturating_add(payload_bytes);
        if page_end > bytes.len() {
            break;
        }

        let serial = u32::from_le_bytes(bytes[page + 14..page + 18].try_into().unwrap());
        let granule = u64::from_le_bytes(bytes[page + 6..page + 14].try_into().unwrap());
        if granule != u64::MAX {
            page_granules
                .entry(serial)
                .and_modify(|value| *value = (*value).max(granule))
                .or_insert(granule);
        }

        let mut segment_offset = payload_start;
        for segment_length in &bytes[lacing_start..payload_start] {
            let length = *segment_length as usize;
            if !completed_serials.contains(&serial) {
                let packet = first_packet_by_serial.entry(serial).or_default();
                if packet.len().saturating_add(length) <= 1024 * 1024 {
                    packet.extend_from_slice(&bytes[segment_offset..segment_offset + length]);
                }
                if length < 255 {
                    if let Ok(header) = read_header_ident(packet) {
                        let index = links.len();
                        links.push(VorbisLink::from_header(serial, &header));
                        link_index_by_serial.insert(serial, index);
                    }
                    completed_serials.insert(serial);
                    first_packet_by_serial.remove(&serial);
                }
            }
            segment_offset += length;
        }
        page = page_end;
    }

    for (serial, index) in link_index_by_serial {
        links[index].total_samples = page_granules.get(&serial).copied();
    }
    links
}

fn open_reader(bytes: &[u8]) -> Result<OggStreamReader<Cursor<Vec<u8>>>, VorbisError> {
    OggStreamReader::new(Cursor::new(bytes.to_vec()))
}

fn ov_open_callbacks(p: &mut Process) -> R {
    if p.cpu.mode != CpuMode::X86_32 {
        return Err(Stop::Unimplemented {
            dll: "vorbisfile".into(),
            name: "ov_open_callbacks only supports the 32-bit ov_callbacks ABI".into(),
        });
    }
    let (datasource, vf, initial, initial_bytes_raw, callbacks_ptr) = (
        p.arg(0),
        p.arg(1),
        p.arg(2),
        p.arg(3) as u32 as i32,
        p.arg(4),
    );
    if vf == 0 || p.extra.vorbisfile.files.contains_key(&vf) {
        return Ok(signed_result(OV_EINVAL));
    }
    if initial_bytes_raw < -1 {
        return Ok(signed_result(OV_EINVAL));
    }
    let callbacks = match read_callbacks(p, callbacks_ptr) {
        Ok(callbacks) if callbacks.read != 0 => callbacks,
        Ok(_) => return Ok(signed_result(OV_EINVAL)),
        Err(error) => return Err(error),
    };
    let initial_bytes = initial_bytes_raw.max(0) as usize;
    let compressed = match read_callback_source(p, datasource, callbacks, initial, initial_bytes)? {
        Ok(compressed) => compressed,
        Err(code) => return Ok(signed_result(code)),
    };
    if !compressed.starts_with(b"OggS") {
        return Ok(signed_result(OV_ENOTVORBIS));
    }

    let links = parse_ogg_links(&compressed);
    let decoder = match open_reader(&compressed) {
        Ok(decoder) => decoder,
        Err(error) => return Ok(signed_result(map_decoder_error(&error))),
    };
    let Some(first_link) = links.first() else {
        return Ok(signed_result(OV_ENOTVORBIS));
    };
    if first_link.serial != decoder.stream_serial() {
        // lewton deliberately supports a single active Vorbis logical stream at a time. If the
        // first Vorbis serial cannot be reconciled with its validated header, reject the stream.
        return Ok(signed_result(OV_ENOTVORBIS));
    }

    let pending_channels = first_link.channels as usize;
    let pending_rate = first_link.sample_rate;
    let seekable = source_is_seekable(p, datasource, callbacks)?;
    let info_pointers = vec![0; links.len()];
    let file = VorbisFile {
        compressed,
        decoder,
        datasource,
        callbacks,
        seekable,
        links,
        pending_pcm: Vec::new(),
        pending_offset: 0,
        pending_link: 0,
        pending_channels,
        pending_rate,
        position_seconds: 0.0,
        info_pointers,
    };
    p.extra.vorbisfile.files.insert(vf, file);
    Ok(0)
}

fn ov_clear(p: &mut Process) -> R {
    let vf = p.arg(0);
    let Some(file) = p.extra.vorbisfile.files.remove(&vf) else {
        return Ok(signed_result(OV_EINVAL));
    };
    if file.callbacks.close != 0 {
        let _ = p.call_guest(file.callbacks.close, &[file.datasource])?;
    }
    Ok(0)
}

fn total_seconds(file: &VorbisFile, link: i32) -> Option<f64> {
    if !file.seekable {
        return None;
    }
    if link == -1 {
        let mut total = 0.0;
        for item in &file.links {
            total += item.duration_seconds()?;
        }
        return Some(total);
    }
    file.links.get(link as usize)?.duration_seconds()
}

fn push_x87_result(p: &mut Process, value: f64) -> u64 {
    let new_top = p.cpu.fpu_top.wrapping_sub(1) & 7;
    p.cpu.fpu_top = new_top;
    p.cpu.fpu_st[new_top as usize] = value;
    p.cpu.fpu_tw &= !(3 << (new_top * 2));
    0
}

fn ov_time_total(p: &mut Process) -> R {
    let (vf, link) = (p.arg(0), p.arg(1) as u32 as i32);
    let Some(file) = p.extra.vorbisfile.files.get(&vf) else {
        return Ok(push_x87_result(p, -1.0));
    };
    let value = total_seconds(file, link).unwrap_or(-1.0);
    Ok(push_x87_result(p, value))
}

fn ov_time_tell(p: &mut Process) -> R {
    let vf = p.arg(0);
    let value = p
        .extra
        .vorbisfile
        .files
        .get(&vf)
        .map(|file| file.position_seconds)
        .unwrap_or(-1.0);
    Ok(push_x87_result(p, value))
}

fn current_link_index(file: &VorbisFile) -> usize {
    let serial = if file.pending_offset < file.pending_pcm.len() {
        file.links
            .get(file.pending_link)
            .map(|link| link.serial)
            .unwrap_or_else(|| file.decoder.stream_serial())
    } else {
        file.decoder.stream_serial()
    };
    file.links
        .iter()
        .position(|link| link.serial == serial)
        .unwrap_or(0)
}

fn write_vorbis_info(
    p: &mut Process,
    vf: u64,
    link: usize,
    file: &mut VorbisFile,
) -> Result<u64, Stop> {
    let Some(metadata) = file.links.get(link) else {
        return Ok(0);
    };
    let mut address = file.info_pointers[link];
    if address == 0 {
        address = super::data_cell(p, &format!("vorbis_info_{vf:x}_{link}"), 32);
        file.info_pointers[link] = address;
    }
    let fields = [
        0i32,
        metadata.channels as i32,
        metadata.sample_rate as i32,
        metadata.bitrate_maximum,
        metadata.bitrate_nominal,
        metadata.bitrate_minimum,
        0, // bitrate_window is unspecified in the Vorbis identification header.
        0, // codec_setup is opaque decoder-owned state, not exposed to guest code.
    ];
    let bytes: Vec<u8> = fields
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    p.write_bytes(address, &bytes)?;
    Ok(address)
}

fn ov_info(p: &mut Process) -> R {
    let (vf, link) = (p.arg(0), p.arg(1) as u32 as i32);
    let Some(mut file) = p.extra.vorbisfile.files.remove(&vf) else {
        return Ok(0);
    };
    let link_index = if link == -1 {
        current_link_index(&file)
    } else if link >= 0 {
        link as usize
    } else {
        usize::MAX
    };
    let result = write_vorbis_info(p, vf, link_index, &mut file);
    p.extra.vorbisfile.files.insert(vf, file);
    result
}

fn decode_next_packet(file: &mut VorbisFile) -> Result<bool, VorbisError> {
    let Some(pcm) = file.decoder.read_dec_packet_itl()? else {
        return Ok(false);
    };
    let serial = file.decoder.stream_serial();
    let Some(link_index) = file.links.iter().position(|link| link.serial == serial) else {
        return Err(VorbisError::BadHeader(HeaderReadError::HeaderBadFormat));
    };
    let link = &file.links[link_index];
    file.pending_pcm = pcm;
    file.pending_offset = 0;
    file.pending_link = link_index;
    file.pending_channels = link.channels as usize;
    file.pending_rate = link.sample_rate;
    Ok(true)
}

fn ov_read(p: &mut Process) -> R {
    let (vf, output, length, big_endian, word_size, signed, bitstream_out) = (
        p.arg(0),
        p.arg(1),
        p.arg(2) as u32 as i32,
        p.arg(3) as u32,
        p.arg(4) as u32,
        p.arg(5) as u32,
        p.arg(6),
    );
    if output == 0
        || length < 0
        || !matches!(big_endian, 0 | 1)
        || !matches!(word_size, 1 | 2)
        || !matches!(signed, 0 | 1)
    {
        return Ok(signed_result(OV_EINVAL));
    }
    if length == 0 {
        return Ok(0);
    }
    let Some(mut file) = p.extra.vorbisfile.files.remove(&vf) else {
        return Ok(signed_result(OV_EINVAL));
    };
    if file.pending_offset >= file.pending_pcm.len() {
        file.pending_pcm.clear();
        file.pending_offset = 0;
        loop {
            match decode_next_packet(&mut file) {
                Ok(true) if file.pending_pcm.is_empty() => continue,
                Ok(true) => break,
                Ok(false) => {
                    p.extra.vorbisfile.files.insert(vf, file);
                    return Ok(0);
                }
                Err(error) => {
                    p.extra.vorbisfile.files.insert(vf, file);
                    return Ok(signed_result(map_decoder_error(&error)));
                }
            }
        }
    }

    if file.pending_offset >= file.pending_pcm.len() {
        p.extra.vorbisfile.files.insert(vf, file);
        return Ok(0);
    }
    let channels = file.pending_channels.max(1);
    let bytes_per_sample = word_size as usize;
    let frame_bytes = channels.saturating_mul(bytes_per_sample);
    let max_frames = (length as usize) / frame_bytes;
    if max_frames == 0 {
        p.extra.vorbisfile.files.insert(vf, file);
        return Ok(0);
    }
    let remaining_frames = (file.pending_pcm.len() - file.pending_offset) / channels;
    let frames = remaining_frames.min(max_frames);
    let samples = frames * channels;
    let link_index = file.pending_link;
    let mut bytes = Vec::with_capacity(samples * bytes_per_sample);
    for sample in &file.pending_pcm[file.pending_offset..file.pending_offset + samples] {
        if word_size == 2 {
            let encoded = if signed == 1 {
                *sample
            } else {
                sample.wrapping_add(i16::MIN)
            };
            let sample_bytes = if big_endian == 1 {
                encoded.to_be_bytes()
            } else {
                encoded.to_le_bytes()
            };
            bytes.extend_from_slice(&sample_bytes);
        } else {
            let sample8 = if signed == 1 {
                (*sample >> 8) as i8 as u8
            } else {
                ((*sample as i32 + 32768) >> 8) as u8
            };
            bytes.push(sample8);
        }
    }
    file.pending_offset += samples;
    if file.pending_rate != 0 {
        file.position_seconds += frames as f64 / file.pending_rate as f64;
    }
    p.extra.vorbisfile.files.insert(vf, file);
    p.write_bytes(output, &bytes)?;
    if bitstream_out != 0 {
        p.wr32(bitstream_out, link_index as u32)?;
    }
    Ok(bytes.len() as u64)
}

fn seek_to_time(file: &mut VorbisFile, seconds: f64) -> Result<(), VorbisError> {
    let mut decoder = open_reader(&file.compressed)?;
    let mut elapsed = 0.0f64;
    loop {
        let Some(pcm) = decoder.read_dec_packet_itl()? else {
            file.decoder = decoder;
            file.pending_pcm.clear();
            file.pending_offset = 0;
            file.position_seconds = elapsed;
            return Ok(());
        };
        let serial = decoder.stream_serial();
        let Some(link_index) = file.links.iter().position(|link| link.serial == serial) else {
            return Err(VorbisError::BadHeader(HeaderReadError::HeaderBadFormat));
        };
        let link = &file.links[link_index];
        let channels = link.channels as usize;
        let rate = link.sample_rate;
        let frames = pcm.len() / channels.max(1);
        let duration = frames as f64 / rate as f64;
        if elapsed + duration >= seconds {
            let skip = ((seconds - elapsed).max(0.0) * rate as f64).floor() as usize;
            let skip = skip.min(frames);
            file.pending_pcm = pcm;
            file.pending_offset = skip.saturating_mul(channels);
            file.pending_link = link_index;
            file.pending_channels = channels;
            file.pending_rate = rate;
            file.decoder = decoder;
            file.position_seconds = elapsed + skip as f64 / rate as f64;
            return Ok(());
        }
        elapsed += duration;
    }
}

fn ov_time_seek(p: &mut Process) -> R {
    let vf = p.arg(0);
    let bits = (p.arg(1) as u32 as u64) | ((p.arg(2) as u32 as u64) << 32);
    let seconds = f64::from_bits(bits);
    let Some(file) = p.extra.vorbisfile.files.get(&vf) else {
        return Ok(signed_result(OV_EINVAL));
    };
    if !file.seekable {
        return Ok(signed_result(OV_ENOSEEK));
    }
    let Some(total) = total_seconds(file, -1) else {
        return Ok(signed_result(OV_EINVAL));
    };
    if !seconds.is_finite() || seconds < 0.0 || seconds > total {
        return Ok(signed_result(OV_EINVAL));
    }
    let Some(mut file) = p.extra.vorbisfile.files.remove(&vf) else {
        return Ok(signed_result(OV_EINVAL));
    };
    let result = match seek_to_time(&mut file, seconds) {
        Ok(()) => 0,
        Err(error) => map_seek_error(&error),
    };
    p.extra.vorbisfile.files.insert(vf, file);
    Ok(signed_result(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anywasm_cpu::cpu::RSP;
    use anywasm_cpu::mem::{PROT_RW, PROT_RWX};

    const TEST_VF: u64 = 0x1000_0000;

    fn x86_process() -> Process {
        let mut process = Process::new(crate::vfs::Vfs::new());
        process.cpu.mode = CpuMode::X86_32;
        process.vmem_next = 0x1000_0000;
        process
    }

    fn set_x86_args(process: &mut Process, args: &[u32]) {
        let stack = process.alloc_virtual(0x1000, PROT_RW) + 0x800;
        process.wr32(stack, 0xfeed).unwrap();
        for (index, value) in args.iter().enumerate() {
            process.wr32(stack + 4 + index as u64 * 4, *value).unwrap();
        }
        process.cpu.mode = CpuMode::X86_32;
        process.cpu.gpr[RSP] = stack;
    }

    fn test_file(bytes: &[u8], seekable: bool) -> VorbisFile {
        let links = parse_ogg_links(bytes);
        let decoder = open_reader(bytes).expect("fixture decoder");
        let info_pointers = vec![0; links.len()];
        VorbisFile {
            compressed: bytes.to_vec(),
            decoder,
            datasource: 0,
            callbacks: GuestCallbacks::default(),
            seekable,
            links,
            pending_pcm: Vec::new(),
            pending_offset: 0,
            pending_link: 0,
            pending_channels: 2,
            pending_rate: 44_100,
            position_seconds: 0.0,
            info_pointers,
        }
    }

    #[test]
    fn ogg_link_scanner_reads_identification_and_granule_duration() {
        let fixture = include_bytes!("../../tests/data/vorbis_440hz_50ms.ogg");
        let links = parse_ogg_links(fixture);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].channels, 2);
        assert_eq!(links[0].sample_rate, 44_100);
        let frames = links[0].total_samples.expect("final Ogg granule position");
        assert!((frames as i64 - 2_205).abs() <= 1);
    }

    #[test]
    fn lewton_decodes_fixture_to_interleaved_signed_pcm() {
        let fixture = include_bytes!("../../tests/data/vorbis_440hz_50ms.ogg");
        let mut decoder = open_reader(fixture).expect("valid Vorbis stream");
        assert_eq!(decoder.ident_hdr.audio_channels, 2);
        assert_eq!(decoder.ident_hdr.audio_sample_rate, 44_100);
        let mut decoded_samples = 0usize;
        while let Some(packet) = decoder.read_dec_packet_itl().unwrap() {
            decoded_samples += packet.len();
        }
        assert!(decoded_samples >= 4_400);
        assert_eq!(decoded_samples % 2, 0);
    }

    #[test]
    fn x87_double_result_pushes_st0_without_touching_xmm() {
        let mut process = x86_process();
        process.cpu.fpu_top = 3;
        process.cpu.fpu_tw = u16::MAX;
        process.cpu.xmm[0] = 0x1122;

        assert_eq!(push_x87_result(&mut process, 1.25), 0);
        assert_eq!(process.cpu.fpu_top, 2);
        assert_eq!(process.cpu.fpu_st[2], 1.25);
        assert_eq!((process.cpu.fpu_tw >> 4) & 3, 0);
        assert_eq!(process.cpu.xmm[0], 0x1122);
    }

    #[test]
    fn ov_read_writes_real_little_endian_signed_16_bit_pcm_incrementally() {
        let fixture = include_bytes!("../../tests/data/vorbis_440hz_50ms.ogg");
        let mut process = x86_process();
        let output = process.alloc_virtual(0x1000, PROT_RW);
        let bitstream = process.alloc_virtual(4, PROT_RW);
        process
            .extra
            .vorbisfile
            .files
            .insert(TEST_VF, test_file(fixture, true));

        set_x86_args(
            &mut process,
            &[
                TEST_VF as u32,
                output as u32,
                0x400,
                0, // little endian
                2, // signed 16-bit samples
                1,
                bitstream as u32,
            ],
        );
        let count = ov_read(&mut process).unwrap() as usize;
        assert!(count > 0 && count <= 0x400);
        assert_eq!(count % 4, 0, "stereo 16-bit output stays frame aligned");
        assert_eq!(process.rd32(bitstream).unwrap(), 0);
        let pcm = process.read_bytes(output, count).unwrap();
        assert!(pcm.iter().any(|sample| *sample != 0));
        assert!(process.extra.vorbisfile.files[&TEST_VF].position_seconds > 0.0);
    }

    #[test]
    fn ov_time_seek_restarts_and_discards_pcm_to_the_requested_sample() {
        let fixture = include_bytes!("../../tests/data/vorbis_440hz_50ms.ogg");
        let mut process = x86_process();
        process
            .extra
            .vorbisfile
            .files
            .insert(TEST_VF, test_file(fixture, true));

        let target = 0.025f64;
        let bits = target.to_bits();
        set_x86_args(
            &mut process,
            &[TEST_VF as u32, bits as u32, (bits >> 32) as u32],
        );
        assert_eq!(ov_time_seek(&mut process).unwrap() as u32 as i32, 0);
        let actual = process.extra.vorbisfile.files[&TEST_VF].position_seconds;
        assert!((actual - target).abs() < 1.0 / 44_100.0);

        set_x86_args(
            &mut process,
            &[
                TEST_VF as u32,
                0, // ov_info current stream
            ],
        );
        let info = ov_info(&mut process).unwrap();
        assert_ne!(info, 0);
        assert_eq!(process.rd32(info + 4).unwrap(), 2);
        assert_eq!(process.rd32(info + 8).unwrap(), 44_100);
    }

    #[test]
    fn ov_time_total_and_tell_return_x87_seconds() {
        let fixture = include_bytes!("../../tests/data/vorbis_440hz_50ms.ogg");
        let mut process = x86_process();
        process
            .extra
            .vorbisfile
            .files
            .insert(TEST_VF, test_file(fixture, true));

        set_x86_args(&mut process, &[TEST_VF as u32, u32::MAX]);
        assert_eq!(ov_time_total(&mut process).unwrap(), 0);
        let total = process.cpu.fpu_st[7];
        assert!((total - 0.05).abs() < 1.0 / 44_100.0);

        set_x86_args(&mut process, &[TEST_VF as u32]);
        assert_eq!(ov_time_tell(&mut process).unwrap(), 0);
        assert_eq!(process.cpu.fpu_st[6], 0.0);

        let output = process.alloc_virtual(0x1000, PROT_RW);
        let bitstream = process.alloc_virtual(4, PROT_RW);
        set_x86_args(
            &mut process,
            &[
                TEST_VF as u32,
                output as u32,
                0x400,
                0,
                2,
                1,
                bitstream as u32,
            ],
        );
        assert!(ov_read(&mut process).unwrap() > 0);
        set_x86_args(&mut process, &[TEST_VF as u32]);
        assert_eq!(ov_time_tell(&mut process).unwrap(), 0);
        assert!(process.cpu.fpu_st[5] > 0.0);
    }

    #[test]
    fn ov_open_callbacks_reads_from_guest_cdecl_callback_and_ov_clear_forgets_state() {
        let fixture = include_bytes!("../../tests/data/vorbis_440hz_50ms.ogg");
        let mut process = x86_process();
        let input = process.alloc_virtual(fixture.len() as u64, PROT_RW);
        process.write_bytes(input, fixture).unwrap();
        let datasource = process.alloc_virtual(8, PROT_RW);
        process.wr32(datasource, input as u32).unwrap();
        process.wr32(datasource + 4, fixture.len() as u32).unwrap();
        let reader = process.alloc_virtual(64, PROT_RWX);
        let code: &[u8] = &[
            0x55, 0x8b, 0xec, 0x53, 0x56, 0x57, 0x8b, 0x7d, 0x08, 0x8b, 0x4d, 0x14, 0x8b, 0x31,
            0x8b, 0x45, 0x10, 0x3b, 0x41, 0x04, 0x76, 0x03, 0x8b, 0x41, 0x04, 0x89, 0xc3, 0x29,
            0x41, 0x04, 0x01, 0x19, 0x89, 0xd9, 0xf3, 0xa4, 0x89, 0xd8, 0x5f, 0x5e, 0x5b, 0x5d,
            0xc3,
        ];
        process.write_bytes(reader, code).unwrap();
        let callbacks = process.alloc_virtual(OV_CALLBACKS_X86_SIZE as u64, PROT_RW);
        process.wr32(callbacks, reader as u32).unwrap();
        process.write_bytes(callbacks + 4, &[0; 12]).unwrap();
        let vf = process.alloc_virtual(0x2d0, PROT_RW);
        set_x86_args(
            &mut process,
            &[datasource as u32, vf as u32, 0, 0, callbacks as u32],
        );
        assert_eq!(ov_open_callbacks(&mut process).unwrap(), 0);
        assert!(process.extra.vorbisfile.files.contains_key(&vf));
        assert_eq!(process.rd32(datasource + 4).unwrap(), 0);

        set_x86_args(&mut process, &[vf as u32]);
        assert_eq!(ov_clear(&mut process).unwrap(), 0);
        assert!(!process.extra.vorbisfile.files.contains_key(&vf));
    }

    #[test]
    fn vorbis_imports_resolve_as_cdecl_exports() {
        for name in [
            "ov_open_callbacks",
            "ov_clear",
            "ov_time_total",
            "ov_time_tell",
            "ov_read",
            "ov_info",
            "ov_time_seek",
        ] {
            assert!(lookup(name).is_some(), "{name}");
        }
    }
}
