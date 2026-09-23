//! Glyph blit: paint a grid of cells (glyph byte + fg + bg colour) into a
//! 4-bytes-per-pixel buffer using a coverage atlas.
//!
//! Layout contract (shared with `crates/app/src/render`):
//!   * `glyph`, `fg`, `bg` are row-major `cols * rows` cells. A glyph is the
//!     printable ASCII byte itself; anything outside `' '..='~'` draws `'?'`.
//!   * `atlas` holds `atlas_glyphs` boxes of `cell * cell` coverage bytes
//!     (0 = background, 255 = foreground), box `i` = glyph `' ' + i`.
//!   * A colour is a `u32` whose little-endian bytes are the pixel's four
//!     bytes. Each byte is blended independently; which byte is which channel
//!     is the caller's business (the app uses softbuffer's `0x00RRGGBB`).
//!   * `out` is `rows * cell` pixel rows of `stride_px` pixels, 4 bytes each.
//!     Only the first `cols * cell` pixels of each row are written.
//!
//! Pure function, no allocation. Threading happens outside: the caller hands
//! each worker a band of whole cell rows, so bands never share output bytes.
const std = @import("std");

/// Number of glyph boxes in an atlas: printable ASCII `' '..='~'`.
pub const atlas_glyphs: usize = 95;
const first_glyph: u8 = ' ';
const fallback_glyph: u8 = '?';

inline fn glyphIndex(byte: u8) usize {
    const b = if (byte < first_glyph or byte > '~') fallback_glyph else byte;
    return b - first_glyph;
}

inline fn colorBytes(packed_rgba: u32) [4]u8 {
    return std.mem.toBytes(std.mem.nativeToLittle(u32, packed_rgba));
}

/// `(fg * cov + bg * (255 - cov) + 127) / 255` per channel: exact for cov 0
/// and 255, rounds to nearest otherwise. The Rust reference uses the same formula.
inline fn blendChannel(f: u8, b: u8, cov: u8) u8 {
    const c: u32 = cov;
    return @intCast((@as(u32, f) * c + @as(u32, b) * (255 - c) + 127) / 255);
}

pub fn blitCells(
    glyph: []const u8,
    fg: []const u32,
    bg: []const u32,
    cols: usize,
    rows: usize,
    atlas: []const u8,
    cell: usize,
    out: []u8,
    stride_px: usize,
) void {
    std.debug.assert(glyph.len == cols * rows);
    std.debug.assert(fg.len == glyph.len and bg.len == glyph.len);
    std.debug.assert(atlas.len == atlas_glyphs * cell * cell);
    std.debug.assert(cols * cell <= stride_px);
    std.debug.assert(out.len >= rows * cell * stride_px * 4);
    const stride_bytes = stride_px * 4;
    const box = cell * cell;

    // Pixel-row-major so every write streams left to right through memory.
    for (0..rows) |r| {
        const cells_g = glyph[r * cols ..][0..cols];
        const cells_fg = fg[r * cols ..][0..cols];
        const cells_bg = bg[r * cols ..][0..cols];
        for (0..cell) |y| {
            const line = out[(r * cell + y) * stride_bytes ..][0 .. cols * cell * 4];
            for (cells_g, cells_fg, cells_bg, 0..) |g, f, b, c| {
                const cov = atlas[glyphIndex(g) * box + y * cell ..][0..cell];
                const px = line[c * cell * 4 ..][0 .. cell * 4];
                blendRow(cov, colorBytes(f), colorBytes(b), px);
            }
        }
    }
}

fn blendRow(cov: []const u8, fgb: [4]u8, bgb: [4]u8, px: []u8) void {
    std.debug.assert(px.len == cov.len * 4);
    for (cov, 0..) |cv, x| {
        const p = px[x * 4 ..][0..4];
        if (cv == 0) {
            p.* = bgb;
        } else if (cv == 255) {
            p.* = fgb;
        } else {
            for (p, fgb, bgb) |*o, f, b| o.* = blendChannel(f, b, cv);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests: compare against a per-pixel reference on sizes that are not
// multiples of anything, with a stride wider than the painted area.

fn refPixel(glyph: u8, fg: u32, bg: u32, atlas: []const u8, cell: usize, x: usize, y: usize) [4]u8 {
    const cov = atlas[glyphIndex(glyph) * cell * cell + y * cell + x];
    const f = colorBytes(fg);
    const b = colorBytes(bg);
    var p: [4]u8 = undefined;
    for (&p, f, b) |*o, fc, bc| {
        o.* = @intCast((@as(u32, fc) * cov + @as(u32, bc) * (255 - @as(u32, cov)) + 127) / 255);
    }
    return p;
}

test "blit matches per-pixel reference and leaves padding untouched" {
    const cell = 7;
    const cols = 5;
    const rows = 3;
    const stride_px = cols * cell + 3;
    var atlas: [atlas_glyphs * cell * cell]u8 = undefined;
    // Deterministic pseudo-random coverage including exact 0 and 255.
    var seed: u32 = 12345;
    for (&atlas) |*a| {
        seed = seed *% 1664525 +% 1013904223;
        const v: u8 = @truncate(seed >> 24);
        a.* = if (v < 60) 0 else if (v > 200) 255 else v;
    }
    const glyph = [_]u8{ ' ', '.', '~', '#', '@', 'A', 'z', '~', 0, 200, '?', '!', 'M', '.', '.' };
    var fg: [cols * rows]u32 = undefined;
    var bg: [cols * rows]u32 = undefined;
    for (&fg, &bg, 0..) |*f, *b, i| {
        f.* = 0xff000000 | @as(u32, @intCast(i * 0x01020304));
        b.* = 0xff000000 | @as(u32, @intCast(0x00fedcba -% i * 0x00030201));
    }
    const sentinel: u8 = 0xEE;
    var out: [rows * cell * stride_px * 4]u8 = undefined;
    @memset(&out, sentinel);

    blitCells(&glyph, &fg, &bg, cols, rows, &atlas, cell, &out, stride_px);

    for (0..rows * cell) |py| {
        for (0..stride_px) |px| {
            const got = out[(py * stride_px + px) * 4 ..][0..4];
            if (px >= cols * cell) {
                try std.testing.expectEqualSlices(u8, &[_]u8{sentinel} ** 4, got);
                continue;
            }
            const r = py / cell;
            const c = px / cell;
            const i = r * cols + c;
            const want = refPixel(glyph[i], fg[i], bg[i], &atlas, cell, px % cell, py % cell);
            try std.testing.expectEqualSlices(u8, &want, got);
        }
    }
}

test "glyph index clamps to '?'" {
    try std.testing.expectEqual(@as(usize, 0), glyphIndex(' '));
    try std.testing.expectEqual(@as(usize, 94), glyphIndex('~'));
    try std.testing.expectEqual(glyphIndex('?'), glyphIndex(0));
    try std.testing.expectEqual(glyphIndex('?'), glyphIndex(127));
    try std.testing.expectEqual(glyphIndex('?'), glyphIndex(255));
}

test "blend endpoints are exact" {
    try std.testing.expectEqual(@as(u8, 10), blendChannel(200, 10, 0));
    try std.testing.expectEqual(@as(u8, 200), blendChannel(200, 10, 255));
    try std.testing.expectEqual(@as(u8, 128), blendChannel(255, 0, 128));
}
