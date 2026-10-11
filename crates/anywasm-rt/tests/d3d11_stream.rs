//! Runs a real mingw-built Direct3D 11 program and checks the GPU command stream it produces.
use anywasm_rt::hle::gpu::{decode, op};
use anywasm_rt::{Process, Stop, Vfs};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../../fixtures/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Run to exit, collecting the GPU stream across presented frames.
fn run_collect(name: &str) -> (Vec<(u32, Vec<u8>)>, String, Stop) {
    let mut vfs = Vfs::new();
    vfs.insert(name, fixture(name));
    let mut p = Process::new(vfs);
    p.load_exe(name, None).unwrap();
    let mut stream = Vec::new();
    let stop = loop {
        let s = p.run(5_000_000);
        stream.extend(std::mem::take(&mut p.extra.gpu.buf));
        match s {
            Stop::StepLimit | Stop::Frame => {}
            other => break other,
        }
    };
    let out = String::from_utf8_lossy(&p.take_stdout()).into_owned();
    (decode(&stream), out, stop)
}

fn u32s(b: &[u8]) -> Vec<u32> {
    b.chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()
}

#[test]
fn triangle_program_produces_the_expected_gpu_stream() {
    let (cmds, out, stop) = run_collect("d3d11_triangle.exe");
    assert_eq!(stop, Stop::Exit(0), "stdout:\n{out}");
    assert!(out.contains("create device hr=00000000 feature_level=b000"), "{out}");
    assert!(out.contains("frames=3"), "{out}");
    for l in ["rtv hr=00000000", "vb hr=00000000", "vs hr=00000000", "ps hr=00000000", "layout hr=00000000"] {
        assert!(out.contains(l), "missing `{l}` in:\n{out}");
    }

    let ops: Vec<u32> = cmds.iter().map(|c| c.0).collect();
    let count = |o: u32| ops.iter().filter(|&&x| x == o).count();
    assert_eq!(count(op::PRESENT), 3, "one PRESENT per frame");
    assert_eq!(count(op::DRAW), 3);
    assert_eq!(count(op::CLEAR_RTV), 3);
    assert_eq!(count(op::CREATE_SHADER), 2);
    assert_eq!(count(op::CREATE_INPUT_LAYOUT), 1);
    assert_eq!(count(op::SWAPCHAIN), 1);

    // Creation commands precede the first draw.
    let first_draw = ops.iter().position(|&o| o == op::DRAW).unwrap();
    for creator in [op::SWAPCHAIN, op::CREATE_SHADER, op::CREATE_INPUT_LAYOUT, op::CREATE_BUFFER, op::CREATE_VIEW] {
        assert!(ops.iter().position(|&o| o == creator).unwrap() < first_draw, "op {creator} after first draw");
    }

    // Shaders are forwarded as untouched DXBC containers (vertex = stage 0, pixel = stage 1).
    let shaders: Vec<_> = cmds.iter().filter(|c| c.0 == op::CREATE_SHADER).collect();
    let stages: Vec<u32> = shaders.iter().map(|c| u32s(&c.1)[1]).collect();
    assert_eq!(stages, vec![0, 1]);
    for s in &shaders {
        assert_eq!(&s.1[12..16], b"DXBC");
    }

    // Input layout: POSITION -> register 0 (R32G32B32_FLOAT=6 @0), COLOR -> register 1 (R32G32B32A32_FLOAT=2 @12).
    let il = cmds.iter().find(|c| c.0 == op::CREATE_INPUT_LAYOUT).unwrap();
    let w = u32s(&il.1);
    assert_eq!(w[1], 2, "two elements");
    assert_eq!(&w[2..8], &[0, 6, 0, 0, 0, 0], "POSITION");
    assert_eq!(&w[8..14], &[1, 2, 0, 12, 0, 0], "COLOR");

    // Vertex data upload matches what the program wrote.
    let writes: Vec<_> = cmds.iter().filter(|c| c.0 == op::BUFFER_WRITE).collect();
    let vb = writes.iter().find(|c| u32s(&c.1)[2] == 3 * 28).expect("vertex buffer upload");
    let floats: Vec<f32> = vb.1[12..12 + 84].chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
    assert_eq!(&floats[0..7], &[0.0, 0.6, 0.0, 1.0, 0.0, 0.0, 1.0]);
    assert_eq!(&floats[7..14], &[0.6, -0.6, 0.0, 0.0, 1.0, 0.0, 1.0]);

    // Frame state: clear color, draw size, vertex stride.
    let clear = cmds.iter().find(|c| c.0 == op::CLEAR_RTV).unwrap();
    let cc: Vec<f32> = u32s(&clear.1)[1..5].iter().map(|b| f32::from_bits(*b)).collect();
    assert_eq!(cc, vec![0.0, 0.0, 0.2, 1.0]);
    let draw = cmds.iter().find(|c| c.0 == op::DRAW).unwrap();
    assert_eq!(u32s(&draw.1), vec![3, 0]);
    let svb = cmds.iter().find(|c| c.0 == op::SET_VERTEX_BUFFERS).unwrap();
    let w = u32s(&svb.1);
    assert_eq!((w[0], w[1], w[3], w[4]), (0, 1, 28, 0), "slot 0, one buffer, stride 28, offset 0");
    let vp = cmds.iter().find(|c| c.0 == op::SET_VIEWPORTS).unwrap();
    let w = u32s(&vp.1);
    assert_eq!(w[0], 1);
    assert_eq!(f32::from_bits(w[3]), 640.0);
    assert_eq!(f32::from_bits(w[4]), 480.0);

    // The dynamic constant buffer is re-uploaded through Map/Unmap every frame (16 bytes of 1.0f).
    let cb_writes = writes.iter().filter(|c| u32s(&c.1)[2] == 16).count();
    assert!(cb_writes >= 3, "constant buffer uploads: {cb_writes}");
}

#[test]
fn textured_program_produces_texture_depth_and_indexed_draw_commands() {
    let (cmds, out, stop) = run_collect("d3d11_textured.exe");
    assert_eq!(stop, Stop::Exit(0), "stdout:\n{out}");
    for line in [
        "device hr=00000000",
        "depth tex hr=00000000",
        "dsv hr=00000000",
        "texture hr=00000000",
        "srv hr=00000000",
        "copy destination hr=00000000",
        "copy srv hr=00000000",
        "copy vertex buffer hr=00000000",
        "sampler hr=00000000",
        "depth state hr=00000000",
        "frames=3",
    ] {
        assert!(out.contains(line), "missing `{line}` in:\n{out}");
    }

    let ops: Vec<u32> = cmds.iter().map(|c| c.0).collect();
    let count = |opcode: u32| ops.iter().filter(|&&op| op == opcode).count();
    assert_eq!(count(op::PRESENT), 3);
    assert_eq!(count(op::DRAW_INDEXED), 15);
    assert_eq!(count(op::COPY_RESOURCE), 2);
    assert_eq!(count(op::CREATE_BUFFER), 4);
    assert_eq!(count(op::CREATE_TEXTURE2D), 3, "depth, source texture, and copy destination");
    assert_eq!(count(op::CLEAR_DSV), 3);
    assert_eq!(count(op::CREATE_SAMPLER), 1);
    assert_eq!(count(op::CREATE_DEPTH), 1);
    assert_eq!(count(op::TEXTURE_WRITE), 2);

    let first_draw = ops
        .iter()
        .position(|&opcode| opcode == op::DRAW_INDEXED)
        .unwrap();
    let texture_ids: Vec<_> = cmds
        .iter()
        .filter(|c| c.0 == op::CREATE_TEXTURE2D)
        .map(|c| u32s(&c.1)[0])
        .collect();
    let buffer_ids: Vec<_> = cmds
        .iter()
        .filter(|c| c.0 == op::CREATE_BUFFER)
        .map(|c| u32s(&c.1)[0])
        .collect();
    let mut texture_copies = 0;
    let mut buffer_copies = 0;
    for (index, copy) in cmds
        .iter()
        .enumerate()
        .filter(|(_, c)| c.0 == op::COPY_RESOURCE)
    {
        let args = u32s(&copy.1);
        assert_eq!(
            args.len(),
            2,
            "COPY_RESOURCE carries destination and source IDs"
        );
        assert_ne!(args[0], args[1], "copy uses distinct resources");
        assert!(index < first_draw, "COPY_RESOURCE must precede drawing");
        if texture_ids.contains(&args[0]) && texture_ids.contains(&args[1]) {
            texture_copies += 1;
        } else if buffer_ids.contains(&args[0]) && buffer_ids.contains(&args[1]) {
            buffer_copies += 1;
        } else {
            panic!("COPY_RESOURCE IDs do not refer to two textures or two buffers: {args:?}");
        }
    }
    assert_eq!(texture_copies, 1);
    assert_eq!(buffer_copies, 1);
    for creator in [
        op::CREATE_TEXTURE2D,
        op::CREATE_VIEW,
        op::CREATE_SHADER,
        op::CREATE_INPUT_LAYOUT,
        op::CREATE_BUFFER,
        op::CREATE_SAMPLER,
        op::CREATE_DEPTH,
    ] {
        assert!(
            ops.iter().position(|&opcode| opcode == creator).unwrap() < first_draw,
            "op {creator} after first draw"
        );
    }

    let texture = cmds.iter().find(|c| c.0 == op::CREATE_TEXTURE2D && u32s(&c.1)[1] == 2).unwrap();
    assert_eq!(&u32s(&texture.1)[1..4], &[2, 2, 1], "2x2, one mip");
    let upload = cmds.iter().find(|c| c.0 == op::TEXTURE_WRITE).unwrap();
    assert_eq!(&u32s(&upload.1)[1..9], &[0, 0, 0, 0, 2, 2, 8, 16]);
    assert_eq!(
        &upload.1[36..52],
        &[
            0xff, 0, 0, 0xff, 0, 0xff, 0, 0xff, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff
        ]
    );

    let index_buffer = cmds.iter().find(|c| c.0 == op::SET_INDEX_BUFFER).unwrap();
    assert_eq!(u32s(&index_buffer.1)[1], 57, "R16_UINT index format");
    let draws: Vec<_> = cmds.iter().filter(|c| c.0 == op::DRAW_INDEXED).collect();
    let expected_frame = [[6, 0, 0], [6, 6, 0], [6, 12, 0], [6, 18, 0], [6, 24, 0]];
    for frame in 0..3 {
        for draw in 0..5 {
            assert_eq!(u32s(&draws[frame * 5 + draw].1), expected_frame[draw]);
        }
    }
}
