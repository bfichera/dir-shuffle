#!/usr/bin/env python3
"""Drive dir-shuffle in a pseudo-terminal, like a real terminal would, for end-to-end tests.

It answers the startup queries the way Ghostty does (kitty graphics + compression,
cell size, cursor position), sends keys, and then:
  * prints the final screen as text (needs `pyte`: run with `uv run --with pyte`),
  * reports kitty transmits/deletes and decodes every transmitted image to PNG.

Examples (from the repo root, after `cargo build --release`):
  uv run --with pyte tools/tui_drive.py /path/to/tree '\\r' j l
  uv run --with pyte tools/tui_drive.py --plain /path/to/tree '\\r'        # no kitty: halfblocks
  uv run --with pyte tools/tui_drive.py --png-dir /tmp/shots /path/to/tree '\\r' j

Keys are Python escape strings: '\\r' Enter, '\\t' Tab, '\\x1b[Z' Shift-Tab, '\\x1b' Esc,
'\\x04' Ctrl-d. Uppercase letters arrive as Shift+letter, as from a real terminal.
In the screen dump, kitty placeholder cells show as '#'.
"""
import argparse, base64, fcntl, os, pty, re, select, struct, sys, termios, time, zlib

APC = re.compile(rb"\x1b_G([^;\x1b]*);?([^\x1b]*)\x1b\\")  # one kitty APC command


def run(binary, args, keys, kitty=True, cols=120, rows=40, cell=(10, 20), delay=0.4):
    pid, fd = pty.fork()
    if pid == 0:
        os.environ["TERM"] = "xterm-256color"
        os.execv(binary, [binary, *args])
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, cols * cell[0], rows * cell[1]))
    out = bytearray()
    state = {"answered": False, "cpr": 0}

    def pump(seconds):
        end = time.time() + seconds
        while time.time() < end:
            r, _, _ = select.select([fd], [], [], 0.01)
            if not r:
                continue
            try:
                data = os.read(fd, 1 << 20)
            except OSError:
                return  # child exited
            out.extend(data)
            # ratatui-image's capability query ends with DSR `ESC[5n`.
            if not state["answered"] and b"\x1b[5n" in out:
                state["answered"] = True
                resp = b"\x1b_Gi=31;OK\x1b\\\x1b_Gi=32;OK\x1b\\" if kitty else b""
                resp += b"\x1b[?62;22c" + b"\x1b[6;%d;%dt" % (cell[1], cell[0]) + b"\x1b[0n"
                os.write(fd, resp)
            # Cursor position requests (ratatui's Terminal::new, the query itself).
            while state["answered"] and out.count(b"\x1b[6n") > state["cpr"]:
                state["cpr"] += 1
                os.write(fd, b"\x1b[1;1R")

    pump(1.0)
    for k in keys:
        os.write(fd, k)
        pump(delay)
    pump(0.8)
    try:
        os.kill(pid, 9)
        os.waitpid(pid, 0)
    except (ProcessLookupError, ChildProcessError):
        pass
    return bytes(out)


def kitty_images(out):
    """Transmitted images as (header dict, raw RGBA bytes, w, h), plus delete ids."""
    imgs, deletes, cur = [], [], None
    for hdr, data in APC.findall(out):
        kv = dict(x.split(b"=", 1) for x in hdr.split(b",") if b"=" in x)
        if kv.get(b"a") == b"T":
            cur = [kv, bytearray(data)]
            imgs.append(cur)
        elif kv.get(b"a") == b"d":
            deletes.append(int(kv[b"i"]))
        elif cur is not None and list(kv) == [b"m"]:
            cur[1].extend(data)
    decoded = []
    for kv, data in imgs:
        w, h = int(kv[b"s"]), int(kv[b"v"])
        raw = base64.b64decode(bytes(data))
        if kv.get(b"o") == b"z":
            raw = zlib.decompress(raw)
        decoded.append((kv, raw, w, h))
    return decoded, deletes


def write_png(path, w, h, rgba):
    rows = b"".join(b"\0" + rgba[y * w * 4:(y + 1) * w * 4] for y in range(h))
    chunk = lambda t, d: struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def screen_text(out, cols, rows):
    import pyte  # uv run --with pyte
    text = APC.sub(b"", out).decode("utf-8", "replace").replace("\U0010EEEE", "#")
    text = re.sub(r"[\u0300-\u036f\u0483-\u0487\u0591-\u05c7\u0610-\u061a\u064b-\u065f\u0670"
                  r"\u06d6-\u06ed\u0711\u0730-\u074a\u07eb-\u07f3\u0816-\u082d\u0951-\u0954"
                  r"\u0f82-\u0f87\u135d-\u135f\u17dd\u193a\u1a17-\u1a75\u1b6b-\u1b73\u1cd0-\u1cf9"
                  r"\u1dc0-\u1dff\u20d0-\u20f0\u2cef-\u2cf1\u2de0-\u2dff\ufe20-\ufe2f"
                  r"\U00010a0f-\U0001d244]", "", text)  # placeholder row/column diacritics
    screen = pyte.Screen(cols, rows)
    pyte.Stream(screen).feed(text)
    return "\n".join(screen.display)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("dir")
    ap.add_argument("keys", nargs="*")
    ap.add_argument("--bin", default=os.environ.get("BIN", os.path.join(
        os.environ.get("CARGO_TARGET_DIR", "target"), "release", "dir-shuffle")))
    ap.add_argument("--plain", action="store_true", help="terminal without kitty graphics")
    ap.add_argument("--cols", type=int, default=120)
    ap.add_argument("--rows", type=int, default=40)
    ap.add_argument("--delay", type=float, default=0.4, help="seconds after each key")
    ap.add_argument("--png-dir", help="decode transmitted kitty images into this directory")
    ap.add_argument("--no-screen", action="store_true", help="skip the screen dump (no pyte)")
    a = ap.parse_args()
    keys = [k.encode().decode("unicode_escape").encode("latin-1") for k in a.keys]
    out = run(a.bin, [a.dir], keys, kitty=not a.plain, cols=a.cols, rows=a.rows, delay=a.delay)
    if not a.no_screen:
        print(screen_text(out, a.cols, a.rows))
    imgs, deletes = kitty_images(out)
    print(f"kitty: {len(imgs)} transmitted, {len(deletes)} deleted; "
          f"ids {[int(kv[b'i']) for kv, *_ in imgs]} deleted {deletes}")
    for i, (kv, raw, w, h) in enumerate(imgs):
        assert len(raw) == w * h * 4, f"image {i}: {len(raw)} bytes for {w}x{h}"
        if a.png_dir:
            os.makedirs(a.png_dir, exist_ok=True)
            p = os.path.join(a.png_dir, f"img{i}.png")
            write_png(p, w, h, raw)
            print(f"  {p}: {w}x{h} px, {kv[b'c'].decode()}x{kv[b'r'].decode()} cells")


if __name__ == "__main__":
    sys.exit(main())
