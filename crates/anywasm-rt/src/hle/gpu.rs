//! Binary GPU/host command stream: everything the browser must do on behalf of the guest
//! (create windows, create GPU objects, set state, draw, present). The guest-side HLE appends
//! commands; the JS executor drains the buffer between run slices.
//!
//! Wire format: repeated `[u32 opcode][u32 payload_len][payload ...]` (little endian, payload
//! padded to a multiple of 4). All ids are small host-assigned u32s; 0 means "none".

pub mod op {
    pub const WINDOW: u32 = 1; // id, w, h, title utf8
    pub const CREATE_BUFFER: u32 = 2; // id, size, bind, usage, misc, stride
    pub const BUFFER_WRITE: u32 = 3; // id, offset, len, bytes
    pub const CREATE_TEXTURE2D: u32 = 4; // id,w,h,mips,array,format,bind,usage,samples,misc
    pub const TEXTURE_WRITE: u32 = 5; // id,mip,slice,x,y,w,h,rowpitch,len,bytes
    pub const CREATE_SHADER: u32 = 6; // id, stage, len, dxbc
    pub const CREATE_INPUT_LAYOUT: u32 = 7; // id, count, then 6 u32 per element
    pub const CREATE_SAMPLER: u32 = 8; // id, 14 u32/f32
    pub const CREATE_BLEND: u32 = 9; // id, raw desc bytes
    pub const CREATE_RASTER: u32 = 10; // id, raw desc bytes
    pub const CREATE_DEPTH: u32 = 11; // id, raw desc bytes
    pub const CREATE_VIEW: u32 = 12; // id, kind, resource, format, dimension, mostDetailedMip, mipLevels, firstSlice, arraySize
    pub const SET_VERTEX_BUFFERS: u32 = 13; // start, count, 3 u32 each (buffer, stride, offset)
    pub const SET_INDEX_BUFFER: u32 = 14; // id, format, offset
    pub const SET_INPUT_LAYOUT: u32 = 15; // id
    pub const SET_TOPOLOGY: u32 = 16; // topology
    pub const SET_SHADER: u32 = 17; // stage, id
    pub const SET_CBUFFERS: u32 = 18; // stage, start, count, ids
    pub const SET_SRVS: u32 = 19; // stage, start, count, ids
    pub const SET_SAMPLERS: u32 = 20; // stage, start, count, ids
    pub const SET_RENDER_TARGETS: u32 = 21; // count, rtv ids..., dsv
    pub const SET_VIEWPORTS: u32 = 22; // count, 6 f32 each
    pub const SET_SCISSORS: u32 = 23; // count, 4 i32 each
    pub const SET_BLEND: u32 = 24; // id, factor[4] f32, sample mask
    pub const SET_RASTER: u32 = 25; // id
    pub const SET_DEPTH: u32 = 26; // id, stencil ref
    pub const CLEAR_RTV: u32 = 27; // view, 4 f32
    pub const CLEAR_DSV: u32 = 28; // view, flags, depth f32, stencil
    pub const DRAW: u32 = 29; // vertex count, start vertex
    pub const DRAW_INDEXED: u32 = 30; // index count, start index, base vertex
    pub const DRAW_INSTANCED: u32 = 31; // vcount/inst, inst count, start vertex, start inst
    pub const DRAW_INDEXED_INSTANCED: u32 = 32; // icount/inst, inst count, start index, base vertex, start inst
    pub const COPY_RESOURCE: u32 = 33; // dst, src
    pub const PRESENT: u32 = 35; // swapchain, texture
    pub const RELEASE: u32 = 36; // id
    pub const SWAPCHAIN: u32 = 38; // id, window, texture, w, h, format
    pub const GENERATE_MIPS: u32 = 39; // srv view id
    pub const COPY_REGION: u32 = 40; // dst,dstSub,dx,dy,dz, src,srcSub, sx0,sy0,sz0,sx1,sy1,sz1, hasBox
    pub const RESIZE_SWAPCHAIN: u32 = 41; // id, texture, w, h, format
    pub const DISPATCH: u32 = 42; // x, y, z
    pub const SET_UAVS: u32 = 43; // start, count, (view id, initial count) pairs
    pub const SET_CBUFFERS1: u32 = 44; // stage, start, count, (buffer id, first constant, num constants) triples
    pub const SET_FIXED_FUNCTION: u32 = 45; // D3D9 FVF, viewport, and stage-0/1 ops, arguments, and texture factor
    pub const CREATE_TEXTURE3D: u32 = 46; // id,w,h,depth,mips,format,bind,usage,misc
    pub const RESOLVE_TEXTURE: u32 = 47; // dst,dstSubresource,src,srcSubresource,format
    pub const D3D9_RENDER_STATE: u32 = 48; // D3DRS state,value
    pub const CLEAR_STATE: u32 = 49; // reset D3D11 immediate-context pipeline bindings
    pub const CLEAR_UAV: u32 = 50; // view, float-values flag, 4 raw value words
    pub const PUSH_STATE: u32 = 51; // save immediate-context pipeline state
    pub const POP_STATE: u32 = 52; // restore the most recently saved pipeline state
    pub const CREATE_TEXTURE1D: u32 = 53; // id,w,mips,array,format,bind,usage,misc
}

#[derive(Default)]
pub struct GpuStream {
    pub buf: Vec<u8>,
    next_id: u32,
}

/// Builder for one command's payload.
#[derive(Default)]
pub struct Pk(Vec<u8>);

impl Pk {
    pub fn new() -> Self {
        Pk(Vec::new())
    }
    pub fn u32(mut self, v: u32) -> Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    pub fn i32(self, v: i32) -> Self {
        self.u32(v as u32)
    }
    pub fn f32(mut self, v: f32) -> Self {
        self.0.extend(v.to_le_bytes());
        self
    }
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
    pub fn bytes(mut self, b: &[u8]) -> Self {
        self.0.extend_from_slice(b);
        self
    }
}

impl GpuStream {
    pub fn new_id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }

    /// Emit a command whose payload is `head` followed by `len` bytes read straight from guest
    /// memory into the stream, so bulk data (texture uploads) is copied once instead of three
    /// times. Returns the range of those bytes inside [`Self::buf`]. On a guest read fault the
    /// stream is left unchanged.
    pub fn emit_guest_payload(
        &mut self,
        opcode: u32,
        head: Pk,
        mem: &anywasm_cpu::Memory,
        address: u64,
        len: usize,
    ) -> Result<std::ops::Range<usize>, String> {
        let head = head.0;
        let padded = (head.len() + len + 3) & !3;
        let rollback = self.buf.len();
        self.buf.extend(opcode.to_le_bytes());
        self.buf.extend((padded as u32).to_le_bytes());
        self.buf.extend(&head);
        let start = self.buf.len();
        self.buf.resize(rollback + 8 + padded, 0);
        if let Err(error) = mem.read_into(
            address,
            &mut self.buf[start..start + len],
            anywasm_cpu::Access::Read,
        ) {
            self.buf.truncate(rollback);
            return Err(format!("read fault {address:#x}+{len}: {error:?}"));
        }
        Ok(start..start + len)
    }

    pub fn emit(&mut self, opcode: u32, payload: Pk) {
        let mut p = payload.0;
        while p.len() % 4 != 0 {
            p.push(0);
        }
        self.buf.extend(opcode.to_le_bytes());
        self.buf.extend((p.len() as u32).to_le_bytes());
        self.buf.extend(p);
    }
}

/// Decode a stream into `(opcode, payload)` pairs (used by tests and tooling).
pub fn decode(buf: &[u8]) -> Vec<(u32, Vec<u8>)> {
    let mut out = Vec::new();
    let mut p = 0;
    while p + 8 <= buf.len() {
        let op = u32::from_le_bytes(buf[p..p + 4].try_into().unwrap());
        let n = u32::from_le_bytes(buf[p + 4..p + 8].try_into().unwrap()) as usize;
        out.push((op, buf[p + 8..p + 8 + n].to_vec()));
        p += 8 + n;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_padding() {
        let mut g = GpuStream::default();
        g.emit(op::DRAW, Pk::new().u32(3).u32(0));
        g.emit(
            op::BUFFER_WRITE,
            Pk::new().u32(1).u32(0).u32(3).bytes(&[7, 8, 9]),
        );
        let d = decode(&g.buf);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0], (op::DRAW, vec![3, 0, 0, 0, 0, 0, 0, 0]));
        assert_eq!(d[1].1.len() % 4, 0);
        assert_eq!(&d[1].1[12..15], &[7, 8, 9]);
    }
}
