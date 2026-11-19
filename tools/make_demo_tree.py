#!/usr/bin/env python3
"""Create a synthetic parameter-sweep tree for manual and end-to-end testing.

  python3 tools/make_demo_tree.py /tmp/demo            # small: 3x1x4 dirs, PDFs + PNGs
  python3 tools/make_demo_tree.py /tmp/big --big       # 20,000 files (startup/scroll benchmark)

Layout (small): <root>/start/{mean,median,none}-rejected/gaussian-normalized/{300K,...}/
with beta_hist.pdf, tau.pdf, map.png, a deeper g2_frames/ dir (must be ignored, D=3),
and a stray .DS_Store (ignored by the default `ignore` patterns). Only stdlib is used;
PDFs are hand-written, one landscape page with a coloured box and a text label.
"""
import os, struct, sys, zlib


def pdf(path, label, w=432, h=288):
    content = (f"0.2 0.4 0.8 rg 40 40 {w-80} {h-80} re f "
               f"1 1 1 rg BT /F1 24 Tf 60 {h-70} Td ({label}) Tj ET").encode()
    objs = ["<< /Type /Catalog /Pages 2 0 R >>", "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Contents 4 0 R "
            f"/Resources << /Font << /F1 5 0 R >> >> >>",
            f"<< /Length {len(content)} >>\nstream\n{content.decode()}\nendstream",
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"]
    out, offs = b"%PDF-1.4\n", []
    for i, o in enumerate(objs):
        offs.append(len(out))
        out += f"{i+1} 0 obj\n{o}\nendobj\n".encode()
    x = len(out)
    out += f"xref\n0 {len(objs)+1}\n0000000000 65535 f \n".encode()
    out += b"".join(f"{o:010} 00000 n \n".encode() for o in offs)
    out += f"trailer\n<< /Size {len(objs)+1} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n".encode()
    with open(path, "wb") as f:
        f.write(out)


def png(path, w, h, rgb):
    raw = b"".join(b"\0" + bytes(rgb) * w for _ in range(h))
    chunk = lambda t, d: struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


def small(root):
    for r in ["mean-rejected", "median-rejected", "none-rejected"]:
        for t in ["300K", "364K", "423K", "1000K"]:
            d = os.path.join(root, "start", r, "gaussian-normalized", t)
            os.makedirs(os.path.join(d, "g2_frames"), exist_ok=True)
            pdf(f"{d}/beta_hist.pdf", f"beta {r} {t}")
            pdf(f"{d}/tau.pdf", f"tau {r} {t}")
            png(f"{d}/map.png", 320, 200, (200, (50 + int(t[:-1]) // 10) % 256, 30))
            for i in range(3):
                png(f"{d}/g2_frames/frame_{i}.png", 8, 8, (0, 0, 0))
    open(os.path.join(root, "start", "mean-rejected", ".DS_Store"), "w").write("junk")
    print(os.path.join(root, "start"))


def big(root):
    src = os.path.join(root, ".src")
    os.makedirs(src, exist_ok=True)
    pdf(f"{src}/p.pdf", "big tree")
    png(f"{src}/i.png", 320, 200, (30, 160, 90))
    n = 0
    for a in range(10):
        for b in range(10):
            for c in range(50):
                d = os.path.join(root, "t", f"a{a}-alpha", f"b{b}-beta", f"{c*10}K")
                os.makedirs(d, exist_ok=True)
                for f in ["p1", "p2", "p3"]:
                    os.link(f"{src}/p.pdf", f"{d}/{f}.pdf")
                os.link(f"{src}/i.png", f"{d}/img.png")
                n += 4
    print(f"{os.path.join(root, 't')} ({n} files)")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    (big if "--big" in sys.argv else small)(sys.argv[1])
