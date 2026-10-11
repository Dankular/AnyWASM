#!/usr/bin/env python3
"""Generate the differential-test instruction corpus (asm -> machine code via llvm-mc).

Output: crates/anywasm-cpu/tests/corpus.txt, lines of `hex|asm`. rsi/rdi are reserved by the
harness as memory base/index; rsp is never used as an operand.
"""
import subprocess, sys, itertools, os

R64 = ["rax", "rcx", "rdx", "rbx", "rbp", "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15"]
R32 = ["eax", "ecx", "edx", "ebx", "ebp", "r8d", "r9d", "r10d", "r11d", "r12d", "r13d", "r14d", "r15d"]
R16 = ["ax", "cx", "dx", "bx", "bp", "r8w", "r9w", "r10w", "r11w", "r12w", "r13w", "r14w", "r15w"]
R8 = ["al", "cl", "dl", "bl", "bpl", "r8b", "r9b", "r10b", "r11b", "r12b", "r13b", "r14b", "r15b"]
RH = ["ah", "ch", "dh", "bh"]  # only without REX
REGS = {8: R8, 16: R16, 32: R32, 64: R64}
PTR = {8: "byte ptr", 16: "word ptr", 32: "dword ptr", 64: "qword ptr"}
MEMS = ["[rsi+16]", "[rsi+rdi*2+8]", "[rsi+rdi*4]", "[rsi+rdi*8-4]", "[rsi]"]
CCS = ["o", "no", "b", "ae", "e", "ne", "be", "a", "s", "ns", "p", "np", "l", "ge", "le", "g"]
lines = []

def add(s):
    lines.append(s)

def pairs(sz, n=3):
    r = REGS[sz]
    # a handful of (dst, src) register pairs including same-register and high-register cases
    return [(r[0], r[1]), (r[1], r[2]), (r[3], r[3]), (r[5], r[0]), (r[8], r[12]), (r[4], r[9])][:n + 3]

IMMS = {8: [0, 1, 0x7f, 0x80, 0xff], 16: [0, 1, 0x7fff, 0x8000, 0xffff], 32: [0, 1, 0x7fffffff, 0x80000000, 0xffffffff],
        64: [0, 1, 0x7fffffff, -1, -0x80000000]}

# ── integer ALU ──
for m in ["add", "adc", "sub", "sbb", "and", "or", "xor", "cmp", "test", "mov", "xchg"]:
    for sz in (8, 16, 32, 64):
        for d, s in pairs(sz):
            add(f"{m} {d}, {s}")
        if m != "xchg":
            for imm in IMMS[sz]:
                add(f"{m} {REGS[sz][1]}, {imm}")
        for mem in MEMS[:3]:
            add(f"{m} {REGS[sz][0]}, {PTR[sz]} {mem}")
            add(f"{m} {PTR[sz]} {mem}, {REGS[sz][2]}")
            if m not in ("xchg",):
                add(f"{m} {PTR[sz]} {mem}, {IMMS[sz][1]}")
add("mov ah, bl"); add("mov cl, dh"); add("add ah, ch"); add("mov bh, 5")
add("mov rax, 0x123456789abcdef0"); add("mov r9, 0x8000000000000000"); add("mov eax, 0xdeadbeef")
for m in ["inc", "dec", "neg", "not"]:
    for sz in (8, 16, 32, 64):
        add(f"{m} {REGS[sz][3]}")
        add(f"{m} {PTR[sz]} [rsi+8]")
for m in ["mul", "imul", "div", "idiv"]:
    for sz in (8, 16, 32, 64):
        add(f"{m} {REGS[sz][1]}")          # rcx family only (harness constrains for div/idiv)
        add(f"{m} {PTR[sz]} [rsi+16]")
for sz in (16, 32, 64):
    for d, s in pairs(sz, 2):
        add(f"imul {d}, {s}")
        add(f"imul {d}, {s}, 100")
        add(f"imul {d}, {s}, -3")
    add(f"imul {REGS[sz][2]}, {PTR[sz]} [rsi+rdi*2+8]")
for m in ["shl", "shr", "sar", "rol", "ror", "rcl", "rcr"]:
    for sz in (8, 16, 32, 64):
        r = REGS[sz][2]
        add(f"{m} {r}, 1")
        for c in (2, 3, 7, 15, 31, 63):
            if c < sz or sz == 64:
                add(f"{m} {r}, {c}")
        add(f"{m} {r}, cl")
        add(f"{m} {PTR[sz]} [rsi+8], 3")
        add(f"{m} {PTR[sz]} [rsi+8], cl")
for m in ["shld", "shrd"]:
    for sz in (16, 32, 64):
        a, b = REGS[sz][0], REGS[sz][3]
        # 16-bit counts above 16 are architecturally undefined and differ between CPU vendors, so the
        # corpus only contains the defined range for 16-bit operands.
        for c in ((1, 3, 7, 15, 16) if sz == 16 else (1, 3, 7, 15, 31)):
            add(f"{m} {a}, {b}, {c}")
        add(f"{m} {a}, {b}, cl")
        add(f"{m} {PTR[sz]} [rsi+8], {b}, 5")
for m in ["bt", "bts", "btr", "btc"]:
    for sz in (16, 32, 64):
        a, b = REGS[sz][0], REGS[sz][1]
        add(f"{m} {a}, {b}")
        add(f"{m} {a}, 5")
        add(f"{m} {PTR[sz]} [rsi+8], {b}")      # harness limits rcx for these
        add(f"{m} {PTR[sz]} [rsi+8], 9")
for m in ["bsf", "bsr", "tzcnt", "lzcnt", "popcnt"]:
    for sz in (16, 32, 64):
        add(f"{m} {REGS[sz][0]}, {REGS[sz][1]}")
        add(f"{m} {REGS[sz][2]}, {PTR[sz]} [rsi+16]")
for cc in CCS:
    for sz in (16, 32, 64):
        add(f"cmov{cc} {REGS[sz][0]}, {REGS[sz][1]}")
        add(f"cmov{cc} {REGS[sz][2]}, {PTR[sz]} [rsi+8]")
    add(f"set{cc} {R8[0]}"); add(f"set{cc} {R8[5]}"); add(f"set{cc} byte ptr [rsi+3]")
for sz_dst, sz_src in [(16, 8), (32, 8), (64, 8), (32, 16), (64, 16)]:
    for m in ("movzx", "movsx"):
        add(f"{m} {REGS[sz_dst][0]}, {REGS[sz_src][1]}")
        add(f"{m} {REGS[sz_dst][2]}, {PTR[sz_src]} [rsi+8]")
add("movsxd rax, ecx"); add("movsxd r9, dword ptr [rsi+8]")
add("movzx eax, ah"); add("movsx ecx, bh")
for m in ["lea"]:
    for sz in (16, 32, 64):
        for mem in ["[rsi+rdi*4+100]", "[rax+rcx*2-3]", "[rbx]", "[r8+r9*8]", "[rbp+r10]"]:
            add(f"lea {REGS[sz][0]}, {mem}")
for sz in (8, 16, 32, 64):
    add(f"xadd {REGS[sz][0]}, {REGS[sz][1]}"); add(f"xadd {PTR[sz]} [rsi+8], {REGS[sz][1]}")
    add(f"cmpxchg {REGS[sz][1]}, {REGS[sz][2]}"); add(f"cmpxchg {PTR[sz]} [rsi+8], {REGS[sz][2]}")
for r in ("eax", "ecx", "r9d", "rax", "r12"):
    add(f"bswap {r}")
for x in ["cbw", "cwde", "cdqe", "cwd", "cdq", "cqo", "lahf", "sahf", "cmc", "stc", "clc", "cld", "std", "nop", "pause"]:
    add(x)
add("movbe eax, dword ptr [rsi+8]"); add("movbe qword ptr [rsi+8], rcx"); add("movbe ax, word ptr [rsi]")
add("crc32 eax, cl"); add("crc32 eax, cx"); add("crc32 eax, ecx"); add("crc32 rax, rcx"); add("crc32 eax, byte ptr [rsi+8]")
add("adcx eax, ecx"); add("adox rax, rcx"); add("adcx r9, qword ptr [rsi+8]")
add("cmpxchg8b qword ptr [rsi+8]"); add("cmpxchg16b xmmword ptr [rsi+16]")
add("xchg rax, r9"); add("xchg eax, ecx")
# string instructions (harness sets rsi/rdi/rcx/DF)
for s in ["movsb", "movsw", "movsd", "movsq", "stosb", "stosw", "stosd", "stosq", "lodsb", "lodsw", "lodsd", "lodsq",
          "scasb", "scasw", "scasd", "scasq", "cmpsb", "cmpsw", "cmpsd", "cmpsq"]:
    pass
add("rep movsb byte ptr [rdi], byte ptr [rsi]"); add("rep movsw word ptr [rdi], word ptr [rsi]")
add("rep movsd dword ptr [rdi], dword ptr [rsi]"); add("rep movsq qword ptr [rdi], qword ptr [rsi]")
add("rep stosb byte ptr [rdi], al"); add("rep stosw word ptr [rdi], ax"); add("rep stosd dword ptr [rdi], eax"); add("rep stosq qword ptr [rdi], rax")
add("repne scasb al, byte ptr [rdi]"); add("repe scasb al, byte ptr [rdi]"); add("repne scasd eax, dword ptr [rdi]")
add("repe cmpsb byte ptr [rsi], byte ptr [rdi]"); add("repne cmpsq qword ptr [rsi], qword ptr [rdi]"); add("repe cmpsd dword ptr [rsi], dword ptr [rdi]")
add("lodsb al, byte ptr [rsi]"); add("lodsd eax, dword ptr [rsi]"); add("lodsq rax, qword ptr [rsi]")
add("stosb byte ptr [rdi], al"); add("stosq qword ptr [rdi], rax"); add("movsb byte ptr [rdi], byte ptr [rsi]")
add("scasb al, byte ptr [rdi]"); add("cmpsd dword ptr [rsi], dword ptr [rdi]")

# ── SSE ──
X = [f"xmm{i}" for i in range(16)]
XM = ["xmmword ptr [rsi+16]", "xmmword ptr [rsi+48]"]
S4, S8 = "dword ptr [rsi+16]", "qword ptr [rsi+24]"
def xx(m, mem=XM[0], pair=True):
    add(f"{m} xmm1, xmm2"); add(f"{m} xmm9, xmm12"); add(f"{m} xmm0, xmm0")
    if mem:
        add(f"{m} xmm3, {mem}")

for base in ["add", "sub", "mul", "div", "min", "max"]:
    for suf, mem in (("ps", XM[0]), ("pd", XM[0]), ("ss", S4), ("sd", S8)):
        xx(base + suf, mem)
for suf, mem in (("ps", XM[0]), ("pd", XM[0]), ("ss", S4), ("sd", S8)):
    xx("sqrt" + suf, mem)
for m in ["andps", "andnps", "orps", "xorps", "andpd", "andnpd", "orpd", "xorpd", "unpcklps", "unpckhps", "unpcklpd", "unpckhpd",
          "haddps", "hsubps", "haddpd", "hsubpd", "addsubps", "addsubpd"]:
    xx(m)
for suf, mem in (("ps", XM[0]), ("pd", XM[0]), ("ss", S4), ("sd", S8)):
    for p in range(8):
        add(f"cmp{suf} xmm1, xmm2, {p}"); add(f"cmp{suf} xmm4, {mem}, {p}")
for m, mem in (("comiss", S4), ("ucomiss", S4), ("comisd", S8), ("ucomisd", S8)):
    xx(m, mem)
for m in ["movaps", "movups", "movapd", "movupd", "movdqa", "movdqu"]:
    add(f"{m} xmm1, xmm2"); add(f"{m} xmm3, {XM[0]}"); add(f"{m} {XM[1]}, xmm5")
add("movss xmm1, xmm2"); add("movss xmm3, dword ptr [rsi+16]"); add("movss dword ptr [rsi+32], xmm5")
add("movsd xmm1, xmm2"); add("movsd xmm3, qword ptr [rsi+16]"); add("movsd qword ptr [rsi+32], xmm5")
add("movlps xmm1, qword ptr [rsi+16]"); add("movlps qword ptr [rsi+32], xmm2"); add("movhps xmm1, qword ptr [rsi+16]")
add("movhps qword ptr [rsi+32], xmm2"); add("movlpd xmm1, qword ptr [rsi+16]"); add("movhpd qword ptr [rsi+32], xmm2")
add("movlhps xmm1, xmm2"); add("movhlps xmm1, xmm2"); add("movddup xmm1, xmm2"); add("movddup xmm1, qword ptr [rsi+16]")
add("movshdup xmm1, xmm2"); add("movsldup xmm1, xmm2"); add("lddqu xmm1, xmmword ptr [rsi+17]")
add("movntdq xmmword ptr [rsi+32], xmm3"); add("movntps xmmword ptr [rsi+32], xmm3"); add("movntdqa xmm1, xmmword ptr [rsi+16]")
add("movd xmm1, eax"); add("movd eax, xmm1"); add("movd xmm1, dword ptr [rsi+16]"); add("movd dword ptr [rsi+32], xmm1")
add("movq xmm1, rax"); add("movq rax, xmm1"); add("movq xmm1, xmm2"); add("movq xmm1, qword ptr [rsi+16]"); add("movq qword ptr [rsi+32], xmm1")
add("movmskps eax, xmm1"); add("movmskpd ecx, xmm2"); add("pmovmskb eax, xmm3"); add("pmovmskb r9d, xmm10")
for imm in (0x00, 0x1b, 0xe4, 0xaa, 0x55, 0xc6):
    add(f"shufps xmm1, xmm2, {imm}"); add(f"shufpd xmm1, xmm2, {imm & 3}")
    add(f"pshufd xmm1, xmm2, {imm}"); add(f"pshufhw xmm1, xmm2, {imm}"); add(f"pshuflw xmm1, xmm2, {imm}")
    add(f"pshufd xmm1, {XM[0]}, {imm}")
# conversions
add("cvtsi2ss xmm1, eax"); add("cvtsi2ss xmm1, rax"); add("cvtsi2sd xmm1, eax"); add("cvtsi2sd xmm1, rcx"); add("cvtsi2sd xmm1, dword ptr [rsi+16]")
add("cvtsi2sd xmm1, qword ptr [rsi+16]")
for m in ["cvtss2si", "cvttss2si", "cvtsd2si", "cvttsd2si"]:
    add(f"{m} eax, xmm1"); add(f"{m} rax, xmm2"); add(f"{m} ecx, {S4 if 'ss' in m else S8}")
for m, mem in [("cvtss2sd", S4), ("cvtsd2ss", S8), ("cvtps2pd", S8), ("cvtpd2ps", XM[0]), ("cvtdq2ps", XM[0]), ("cvtps2dq", XM[0]),
               ("cvttps2dq", XM[0]), ("cvtdq2pd", S8), ("cvtpd2dq", XM[0]), ("cvttpd2dq", XM[0])]:
    xx(m, mem)
# packed integer
PI = ["paddb", "paddw", "paddd", "paddq", "psubb", "psubw", "psubd", "psubq", "paddsb", "paddsw", "paddusb", "paddusw",
      "psubsb", "psubsw", "psubusb", "psubusw", "pmullw", "pmulhw", "pmulhuw", "pmuludq", "pmaddwd", "pavgb", "pavgw",
      "pminub", "pmaxub", "pminsw", "pmaxsw", "pcmpeqb", "pcmpeqw", "pcmpeqd", "pcmpgtb", "pcmpgtw", "pcmpgtd", "pand", "pandn",
      "por", "pxor", "psadbw", "packsswb", "packssdw", "packuswb", "punpcklbw", "punpcklwd", "punpckldq", "punpcklqdq",
      "punpckhbw", "punpckhwd", "punpckhdq", "punpckhqdq", "psllw", "pslld", "psllq", "psrlw", "psrld", "psrlq", "psraw", "psrad",
      # SSSE3 / SSE4
      "pshufb", "phaddw", "phaddd", "phaddsw", "phsubw", "phsubd", "phsubsw", "pmaddubsw", "psignb", "psignw", "psignd", "pmulhrsw",
      "pabsb", "pabsw", "pabsd", "pmuldq", "pcmpeqq", "pcmpgtq", "packusdw", "pminsb", "pminsd", "pminuw", "pminud",
      "pmaxsb", "pmaxsd", "pmaxuw", "pmaxud", "pmulld", "phminposuw", "ptest",
      "pmovsxbw", "pmovsxbd", "pmovsxbq", "pmovsxwd", "pmovsxwq", "pmovsxdq", "pmovzxbw", "pmovzxbd", "pmovzxbq", "pmovzxwd", "pmovzxwq", "pmovzxdq"]
for m in PI:
    add(f"{m} xmm1, xmm2"); add(f"{m} xmm9, xmm12"); add(f"{m} xmm0, xmm0")
    if m.startswith("pmovsx") or m.startswith("pmovzx"):
        w = {"bw": "qword", "bd": "dword", "bq": "word", "wd": "qword", "wq": "dword", "dq": "qword"}[m[-2:]]
        add(f"{m} xmm3, {w} ptr [rsi+16]")
    else:
        add(f"{m} xmm3, {XM[0]}")
for m in ["psllw", "pslld", "psllq", "psrlw", "psrld", "psrlq", "psraw", "psrad"]:
    for c in (0, 1, 7, 15, 16, 31, 63, 64, 100):
        add(f"{m} xmm2, {c}")
for c in (0, 1, 4, 8, 15, 16, 20):
    add(f"pslldq xmm2, {c}"); add(f"psrldq xmm2, {c}")
for imm in (0, 1, 4, 8, 15, 16, 20, 31, 32):
    add(f"palignr xmm1, xmm2, {imm}")
for imm in (0, 0x55, 0xaa, 0xff, 0x0f):
    add(f"pblendw xmm1, xmm2, {imm}"); add(f"blendps xmm1, xmm2, {imm & 15}"); add(f"blendpd xmm1, xmm2, {imm & 3}")
add("pblendvb xmm1, xmm2, xmm0"); add("blendvps xmm1, xmm2, xmm0"); add("blendvpd xmm1, xmm2, xmm0")
for mode in (0, 1, 2, 3, 4, 8, 9, 10, 11):
    for suf, mem in (("ps", XM[0]), ("pd", XM[0]), ("ss", S4), ("sd", S8)):
        add(f"round{suf} xmm1, xmm2, {mode}")
for imm in (0xff, 0x31, 0x71, 0xf3, 0x13):
    add(f"dpps xmm1, xmm2, {imm}"); add(f"dppd xmm1, xmm2, {imm & 0x33}")
for imm in (0x00, 0x10, 0x40, 0x9d, 0xe5, 0xff):
    add(f"insertps xmm1, xmm2, {imm}")
for i in range(4):
    add(f"extractps eax, xmm3, {i}"); add(f"pextrd eax, xmm3, {i}"); add(f"pinsrd xmm1, eax, {i}")
add("pextrq rax, xmm3, 1"); add("pinsrq xmm1, rax, 1"); add("pinsrq xmm1, rcx, 0")
for i in range(8):
    add(f"pextrw eax, xmm3, {i}"); add(f"pinsrw xmm1, eax, {i}")
for i in (0, 5, 15):
    add(f"pextrb eax, xmm3, {i}"); add(f"pinsrb xmm1, eax, {i}")
for imm in (0x00, 0x01, 0x02, 0x03, 0x08, 0x0c, 0x0d, 0x4c, 0x18, 0x38, 0x44, 0x46, 0x0a, 0x14, 0x40):
    for m in ("pcmpistri", "pcmpistrm"):
        add(f"{m} xmm1, xmm2, {imm}")
for imm in (0x00, 0x01, 0x0c, 0x4c, 0x18):
    add(f"pcmpestri xmm1, xmm2, {imm}"); add(f"pcmpestrm xmm1, xmm2, {imm}")


# ── x87 (multi-instruction sequences; each ends with fninit so the FPU is clean) ──
Q1, Q2, Q3 = "qword ptr [rsi+16]", "qword ptr [rsi+24]", "qword ptr [rsi+40]"
D1, D2, D3 = "dword ptr [rsi+80]", "dword ptr [rsi+84]", "dword ptr [rsi+88]"
W1 = "word ptr [rsi+96]"
def seq(*stmts):
    add("; ".join(stmts + ("fninit",)))
for op in ["faddp", "fsubp", "fsubrp", "fmulp", "fdivp"]:
    seq(f"fld {Q1}", f"fld {Q2}", f"{op} st(1), st", f"fstp {Q3}")
for op in ["fadd", "fsub", "fsubr", "fmul", "fdiv"]:
    seq(f"fld {Q1}", f"{op} {Q2}", f"fstp {Q3}")
    seq(f"fld {Q1}", f"{op} {D1}", f"fstp {Q3}")
    seq(f"fld {Q1}", f"fld {Q2}", f"{op} st, st(1)", f"fstp {Q3}", f"fstp qword ptr [rsi+48]")
    seq(f"fld {Q1}", f"fld {Q2}", f"{op} st(1), st", f"fstp {Q3}", f"fstp qword ptr [rsi+48]")
for op in ["fiadd", "fisub", "fisubr", "fimul", "fidiv"]:
    seq(f"fld {Q1}", f"{op} {D1}", f"fstp {Q3}")
    seq(f"fld {Q1}", f"{op} {W1}", f"fstp {Q3}")
seq(f"fld {D1}", f"fstp {D3}")
seq(f"fld {Q1}", f"fst {Q3}", f"fstp qword ptr [rsi+48]")
seq(f"fld {Q1}", f"fstp {D3}")
seq(f"fild {W1}", f"fstp {Q3}")
seq(f"fild {D1}", f"fstp {Q3}")
seq(f"fild qword ptr [rsi+104]", f"fstp {Q3}")
for c in ["fld1", "fldz", "fldpi", "fldl2e", "fldl2t", "fldlg2", "fldln2"]:
    seq(c, f"fstp {Q3}")
for op in ["fchs", "fabs", "fsqrt", "frndint"]:
    seq(f"fld {Q1}", op, f"fstp {Q3}")
seq(f"fld {Q1}", f"fld {Q2}", "fxch st(1)", f"fstp {Q3}", f"fstp qword ptr [rsi+48]")
seq(f"fld {Q1}", f"fld {Q2}", f"fld {Q1}", "fxch st(2)", f"fstp {Q3}", f"fstp qword ptr [rsi+48]", f"fstp qword ptr [rsi+56]")
seq(f"fld {Q1}", "fld st(0)", "faddp st(1), st", f"fstp {Q3}")
# comparisons
seq(f"fld {Q1}", f"fld {Q2}", "fcomi st, st(1)", "fstp st(0)", "fstp st(0)")
seq(f"fld {Q1}", f"fld {Q2}", "fucomi st, st(1)", "fstp st(0)", "fstp st(0)")
seq(f"fld {Q1}", f"fld {Q2}", "fcomip st, st(1)", "fstp st(0)")
seq(f"fld {Q1}", f"fld {Q2}", "fucomip st, st(1)", "fstp st(0)")
seq(f"fld {Q1}", f"fld {Q2}", "fcompp", "fnstsw ax")
seq(f"fld {Q1}", f"fcom {Q2}", "fnstsw ax", "fstp st(0)")
seq(f"fld {Q1}", f"fcomp {Q2}", "fnstsw ax")
seq(f"fld {Q1}", f"fcomp {D1}", "fnstsw ax")
seq(f"fld {Q1}", "ftst", "fnstsw ax", "fstp st(0)")
seq(f"fld {Q1}", "fxam", "fnstsw ax", "fstp st(0)")
for cc in ["b", "e", "be", "u", "nb", "ne", "nbe", "nu"]:
    seq(f"fld {Q1}", f"fld {Q2}", f"fcmov{cc} st, st(1)", f"fstp {Q3}", f"fstp qword ptr [rsi+48]")
# integer stores with each rounding mode (cw: RC in bits 11:10)
for rc in (0x037f, 0x077f, 0x0b7f, 0x0f7f):
    seq(f"mov word ptr [rsi+120], {rc}", "fldcw word ptr [rsi+120]", f"fld {Q1}", f"fistp {W1}", f"fld {Q2}", f"fistp {D3}",
        f"fld {Q1}", "fistp qword ptr [rsi+48]", "fnstcw word ptr [rsi+122]")
    seq(f"mov word ptr [rsi+120], {rc}", "fldcw word ptr [rsi+120]", f"fld {Q1}", "frndint", f"fstp {Q3}")
    seq(f"mov word ptr [rsi+120], {rc}", "fldcw word ptr [rsi+120]", f"fld {Q1}", f"fist {D3}", "fstp st(0)")
seq(f"fld {Q1}", f"fisttp {D3}")
seq(f"fld {Q1}", "fisttp qword ptr [rsi+48]")
seq(f"fld {Q1}", f"fisttp {W1}")
seq("fnstcw word ptr [rsi+120]")
seq("fnstsw ax")
seq(f"fld {Q1}", f"fld {Q2}", "fprem", "fnstsw ax", f"fstp {Q3}", "fstp st(0)")
seq(f"fld {Q1}", f"fld {Q2}", "fscale", f"fstp {Q3}", "fstp st(0)")
seq(f"fld {Q1}", f"fld {Q2}", "fyl2x", f"fstp {Q3}")
seq(f"fld {Q1}", "fsin", f"fstp {Q3}")
seq(f"fld {Q1}", "fcos", f"fstp {Q3}")
seq(f"fld {Q1}", "fptan", "fstp st(0)", f"fstp {Q3}")
seq(f"fld {Q1}", f"fld {Q2}", "fpatan", f"fstp {Q3}")
seq("mov word ptr [rsi+124], 4096", "fild word ptr [rsi+124]", f"fld {Q1}", "fdivrp st(1), st", "f2xm1", f"fstp {Q3}")
seq(f"fld {Q1}", "fld st(0)", "fsincos", f"fstp {Q3}", f"fstp qword ptr [rsi+48]", "fstp st(0)")
# memory fxsave/fxrstor roundtrip of xmm state
add("fxsave [rsi+256]; fxrstor [rsi+256]")

# ── assemble ──
def assemble(stmts):
    p = subprocess.run(["llvm-mc", "--triple=x86_64", "-show-encoding"],
                       input=".intel_syntax noprefix\n" + "\n".join(stmts) + "\n", capture_output=True, text=True)
    got = [x for x in p.stdout.splitlines() if "# encoding:" in x]
    if p.returncode != 0 or len(got) != len(stmts):
        return None, p.stderr.strip().splitlines()[:1]
    return "".join(x.split("# encoding: [")[1].split("]")[0].replace("0x", "").replace(",", "") for x in got), None

out, bad = [], 0
for l in lines:
    stmts = [s.strip() for s in l.split(";")]
    enc, err = assemble(stmts)
    if enc is None:
        bad += 1
        print("SKIP:", l, err, file=sys.stderr)
        continue
    out.append((l, enc))
dest = os.path.join(os.path.dirname(__file__), "..", "crates", "anywasm-cpu", "tests", "corpus.txt")
os.makedirs(os.path.dirname(dest), exist_ok=True)
with open(dest, "w") as f:
    for l, b in out:
        f.write(f"{b}|{l}\n")
print(f"{len(out)} entries written ({bad} skipped)")
