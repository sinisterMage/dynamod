/// Minimal read-only ISO 9660 (+Joliet, +Rock Ridge NM) extent locator.
///
/// Resolves a path like /live/root.squashfs to its (byte offset, size) extents by
/// pread()ing the volume descriptors and directory records straight off the block
/// device — no iso9660 mount, no VFS file I/O on the ISO. This exists because
/// iso9660 VFS open/stat/sync can block indefinitely on QEMU ATAPI CD-ROM, and
/// loop devices backed by files on iso9660 hang on some kernels (Debian #1106070).
/// live.zig uses the result to pread the squashfs into tmpfs (see copySquashFromBlockDev).
///
/// Every failure path logs a reason and returns null so the caller can fall back
/// to mounting the ISO; this module never halts.
const std = @import("std");
const kmsg = @import("kmsg.zig");

/// ECMA-119 logical sector: volume descriptors are addressed in 2048-byte sectors
/// and directory records never cross a 2048-byte sector boundary, regardless of
/// the volume's logical block size.
const logical_sector: usize = 2048;
const vd_start_byte: u64 = 16 * logical_sector;
const max_vd_sectors: u32 = 32;
const max_path_depth: usize = 8;
const max_dir_bytes: u64 = 64 << 20;

pub const Extent = struct {
    start_byte: u64,
    size: u64,
};

pub const max_extents = 8;

pub const FileExtents = struct {
    extents: [max_extents]Extent = undefined,
    count: u8 = 0,
    total_size: u64 = 0,

    pub fn slice(self: *const FileExtents) []const Extent {
        return self.extents[0..self.count];
    }
};

/// Locate `path` (absolute, e.g. "/live/root.squashfs") on the ISO 9660 volume on
/// `dev_z` using only pread. Returns null on anything missing/unsupported (reason
/// logged) so the caller can fall back to mounting the ISO.
pub fn locateFile(dev_z: [*:0]const u8, path: []const u8, klog_arg: ?kmsg) ?FileExtents {
    const fd = std.posix.openZ(dev_z, .{ .ACCMODE = .RDONLY, .NOCTTY = true }, 0) catch {
        if (klog_arg) |k| k.warn("live: iso9660: cannot open device for parse", .{});
        return null;
    };
    defer std.posix.close(fd);
    return locateFileInSource(.{ .fd = fd }, path, klog_arg);
}

/// Byte source: a device fd in production, a memory image in tests.
const Source = union(enum) {
    fd: std.posix.fd_t,
    mem: []const u8,
};

fn readFull(src: Source, off: u64, buf: []u8) bool {
    switch (src) {
        .fd => |fd| {
            var done: usize = 0;
            while (done < buf.len) {
                const n = std.posix.pread(fd, buf[done..], off + done) catch return false;
                if (n == 0) return false;
                done += n;
            }
            return true;
        },
        .mem => |m| {
            if (off >= m.len) return false;
            const start: usize = @intCast(off);
            if (m.len - start < buf.len) return false;
            @memcpy(buf, m[start..][0..buf.len]);
            return true;
        },
    }
}

const DirLoc = struct { lba: u32, size: u32 };

const Volume = struct {
    block_size: u32 = 2048,
    pvd_root: ?DirLoc = null,
    joliet_root: ?DirLoc = null,
};

const Namespace = enum { joliet, primary };

fn locateFileInSource(src: Source, path: []const u8, klog_arg: ?kmsg) ?FileExtents {
    const vol = scanVolumeDescriptors(src, klog_arg) orelse return null;

    // Joliet first: always produced by our ISO builders (-J), identical record
    // layout, and the UCS-2BE identifiers preserve case with no version suffixes.
    if (vol.joliet_root) |root| {
        if (walkPath(src, vol.block_size, root, path, .joliet, klog_arg)) |fx| {
            if (klog_arg) |k| k.info("live: iso9660: located {s} via Joliet", .{path});
            return fx;
        }
    }
    if (vol.pvd_root) |root| {
        if (walkPath(src, vol.block_size, root, path, .primary, klog_arg)) |fx| {
            if (klog_arg) |k| k.info("live: iso9660: located {s} via primary descriptor", .{path});
            return fx;
        }
    }

    if (klog_arg) |k| k.warn("live: iso9660: could not locate {s}", .{path});
    return null;
}

fn scanVolumeDescriptors(src: Source, klog_arg: ?kmsg) ?Volume {
    var vol = Volume{};
    var have_block_size = false;
    var buf: [logical_sector]u8 = undefined;

    var i: u32 = 0;
    while (i < max_vd_sectors) : (i += 1) {
        if (!readFull(src, vd_start_byte + @as(u64, i) * logical_sector, &buf)) {
            if (klog_arg) |k| k.warn("live: iso9660: short read in volume descriptor area", .{});
            return null;
        }
        if (!std.mem.eql(u8, buf[1..6], "CD001")) {
            if (i == 0) {
                if (klog_arg) |k| k.warn("live: iso9660: no CD001 signature (not an ISO9660 volume)", .{});
                return null;
            }
            break;
        }
        switch (buf[0]) {
            1 => if (vol.pvd_root == null) {
                if (readBlockSize(&buf, klog_arg)) |bs| {
                    if (!have_block_size) {
                        vol.block_size = bs;
                        have_block_size = true;
                    }
                    vol.pvd_root = rootDirFromVd(&buf);
                }
            },
            2 => if (vol.joliet_root == null and isJolietEscape(buf[88..120])) {
                if (readBlockSize(&buf, klog_arg)) |bs| {
                    if (!have_block_size) {
                        vol.block_size = bs;
                        have_block_size = true;
                    }
                    vol.joliet_root = rootDirFromVd(&buf);
                }
            },
            255 => break,
            else => {},
        }
    }

    if (vol.pvd_root == null and vol.joliet_root == null) {
        if (klog_arg) |k| k.warn("live: iso9660: no usable volume descriptor", .{});
        return null;
    }
    return vol;
}

fn readBlockSize(sector: *const [logical_sector]u8, klog_arg: ?kmsg) ?u32 {
    const bs = std.mem.readInt(u16, sector[128..130], .little);
    if (bs != 512 and bs != 1024 and bs != 2048) {
        if (klog_arg) |k| k.warn("live: iso9660: unsupported logical block size {d}", .{bs});
        return null;
    }
    return bs;
}

fn rootDirFromVd(sector: *const [logical_sector]u8) ?DirLoc {
    switch (parseDirRecord(sector[0..], 156)) {
        .rec => |r| {
            if (r.flags & flag_dir == 0) return null;
            return DirLoc{ .lba = r.extent_lba, .size = r.data_len };
        },
        else => return null,
    }
}

fn isJolietEscape(esc: []const u8) bool {
    // UCS-2 level 1/2/3 escape sequences: %/@ %/C %/E
    return esc.len >= 3 and esc[0] == 0x25 and esc[1] == 0x2F and
        (esc[2] == 0x40 or esc[2] == 0x43 or esc[2] == 0x45);
}

/// Directory record flags (ECMA-119 9.1.6).
const flag_dir: u8 = 0x02;
/// Set on every record of a multi-extent file except the final one.
const flag_multi_extent: u8 = 0x80;

const DirRecord = struct {
    rec_len: u8,
    extent_lba: u32,
    data_len: u32,
    flags: u8,
    name: []const u8,
    /// System-use area (Rock Ridge lives here).
    su: []const u8,
};

const DirRecordResult = union(enum) {
    /// rec_len == 0: the rest of this logical sector is padding.
    padding,
    malformed,
    rec: DirRecord,
};

fn parseDirRecord(buf: []const u8, off: usize) DirRecordResult {
    if (off >= buf.len) return .malformed;
    const rec_len: usize = buf[off];
    if (rec_len == 0) return .padding;
    if (rec_len < 34 or off + rec_len > buf.len) return .malformed;

    const name_len: usize = buf[off + 32];
    if (name_len == 0 or 33 + name_len > rec_len) return .malformed;

    // Extent LBA and data length are both-endian (LE then BE); read the LE half.
    const extent_lba = std.mem.readInt(u32, buf[off + 2 ..][0..4], .little);
    const data_len = std.mem.readInt(u32, buf[off + 10 ..][0..4], .little);
    const flags = buf[off + 25];
    const name = buf[off + 33 ..][0..name_len];

    // One pad byte after an even-length name keeps the system-use area even-aligned.
    const pad: usize = 1 - (name_len & 1);
    const su_start = off + 33 + name_len + pad;
    const rec_end = off + rec_len;
    const su: []const u8 = if (su_start < rec_end) buf[su_start..rec_end] else buf[0..0];

    return .{ .rec = DirRecord{
        .rec_len = buf[off],
        .extent_lba = extent_lba,
        .data_len = data_len,
        .flags = flags,
        .name = name,
        .su = su,
    } };
}

fn isSpecialId(record_name: []const u8) bool {
    // 0x00 = this directory, 0x01 = parent.
    return record_name.len == 1 and (record_name[0] == 0x00 or record_name[0] == 0x01);
}

fn matchJolietName(record_name: []const u8, component: []const u8) bool {
    if (isSpecialId(record_name)) return false;
    var name = record_name;
    // Some producers append a UCS-2 ";version" even in Joliet; strip it.
    var idx: usize = 0;
    while (idx + 1 < name.len) : (idx += 2) {
        if (name[idx] == 0x00 and name[idx + 1] == ';') {
            name = name[0..idx];
            break;
        }
    }
    if (name.len != component.len * 2) return false;
    for (component, 0..) |c, ci| {
        if (name[2 * ci] != 0x00 or name[2 * ci + 1] != c) return false;
    }
    return true;
}

fn matchPlainName(record_name: []const u8, component: []const u8) bool {
    if (isSpecialId(record_name)) return false;
    var name = record_name;
    if (std.mem.indexOfScalar(u8, name, ';')) |i| name = name[0..i];
    if (name.len > 0 and name[name.len - 1] == '.') name = name[0 .. name.len - 1];
    return std.ascii.eqlIgnoreCase(name, component);
}

const RrNameResult = union(enum) {
    /// No NM entry: use the plain ISO identifier.
    none,
    /// Name continues in a CE continuation area (not followed) or overflowed.
    incomplete,
    name: []const u8,
};

/// Extract the Rock Ridge alternate name from a record's system-use area.
/// Only inline NM entries are handled (concatenated across CONTINUE flags);
/// CE continuation areas are not followed.
fn rrName(su_area: []const u8, skip: u8, out: *[256]u8) RrNameResult {
    if (skip >= su_area.len) return .none;
    const su = su_area[skip..];

    var out_len: usize = 0;
    var have_nm = false;
    var expecting_more = false;
    var saw_ce = false;

    var i: usize = 0;
    while (i + 4 <= su.len) {
        const len: usize = su[i + 2];
        if (len < 4 or i + len > su.len) break;
        if (su[i] == 'S' and su[i + 1] == 'T') break;
        if (su[i] == 'C' and su[i + 1] == 'E') saw_ce = true;
        if (su[i] == 'N' and su[i + 1] == 'M' and len >= 5) {
            const nm_flags = su[i + 4];
            // CURRENT/PARENT flag bits mean the name is "." / ".." — not a real name.
            if (nm_flags & 0x06 == 0) {
                const payload = su[i + 5 .. i + len];
                if (out_len + payload.len > out.len) return .incomplete;
                @memcpy(out[out_len..][0..payload.len], payload);
                out_len += payload.len;
                have_nm = true;
                expecting_more = (nm_flags & 0x01) != 0;
            }
        }
        i += len;
    }

    if (have_nm) {
        if (expecting_more) return .incomplete;
        return .{ .name = out[0..out_len] };
    }
    if (saw_ce) return .incomplete;
    return .none;
}

/// Length to skip at the start of every system-use area, from the SP entry
/// on the root directory's "." record (SUSP 5.3).
fn rrSkipLen(root_dot_su: []const u8) u8 {
    if (root_dot_su.len < 7) return 0;
    if (root_dot_su[0] != 'S' or root_dot_su[1] != 'P') return 0;
    if (root_dot_su[2] != 7) return 0;
    if (root_dot_su[4] != 0xBE or root_dot_su[5] != 0xEF) return 0;
    return root_dot_su[6];
}

fn recordMatches(rec: *const DirRecord, component: []const u8, ns: Namespace, rr_skip: u8, klog_arg: ?kmsg) bool {
    switch (ns) {
        .joliet => return matchJolietName(rec.name, component),
        .primary => {
            var scratch: [256]u8 = undefined;
            switch (rrName(rec.su, rr_skip, &scratch)) {
                .name => |n| return std.mem.eql(u8, n, component),
                .incomplete => {
                    if (klog_arg) |k| k.warn("live: iso9660: RR name needs CE continuation (unsupported), trying plain identifier", .{});
                    return matchPlainName(rec.name, component);
                },
                .none => return matchPlainName(rec.name, component),
            }
        },
    }
}

const Found = union(enum) {
    dir: DirLoc,
    file: FileExtents,
};

fn appendExtent(fx: *FileExtents, rec: *const DirRecord, block_size: u32, klog_arg: ?kmsg) bool {
    if (fx.count >= max_extents) {
        if (klog_arg) |k| k.warn("live: iso9660: file has more than {d} extents", .{max_extents});
        return false;
    }
    fx.extents[fx.count] = .{
        .start_byte = @as(u64, rec.extent_lba) * @as(u64, block_size),
        .size = rec.data_len,
    };
    fx.count += 1;
    fx.total_size += rec.data_len;
    return true;
}

fn findInDirectory(
    src: Source,
    block_size: u32,
    dir: DirLoc,
    component: []const u8,
    ns: Namespace,
    rr_skip: u8,
    klog_arg: ?kmsg,
) ?Found {
    if (dir.size == 0 or dir.size > max_dir_bytes) {
        if (klog_arg) |k| k.warn("live: iso9660: directory extent size out of bounds ({d})", .{dir.size});
        return null;
    }

    const base = @as(u64, dir.lba) * @as(u64, block_size);
    var sector_buf: [logical_sector]u8 = undefined;
    // Records of a multi-extent file are consecutive but may continue past a
    // sector boundary, so collection state lives across the sector loop.
    var collecting: ?FileExtents = null;

    var sec_start: u64 = 0;
    while (sec_start < dir.size) : (sec_start += logical_sector) {
        const chunk: usize = @intCast(@min(@as(u64, logical_sector), dir.size - sec_start));
        if (!readFull(src, base + sec_start, sector_buf[0..chunk])) {
            if (klog_arg) |k| k.warn("live: iso9660: directory read failed", .{});
            return null;
        }

        var off: usize = 0;
        while (off < chunk) {
            switch (parseDirRecord(sector_buf[0..chunk], off)) {
                .padding => break,
                .malformed => {
                    if (klog_arg) |k| k.warn("live: iso9660: malformed directory record", .{});
                    return null;
                },
                .rec => |rec| {
                    if (collecting) |*fx| {
                        if (!recordMatches(&rec, component, ns, rr_skip, klog_arg)) {
                            if (klog_arg) |k| k.warn("live: iso9660: multi-extent record sequence broken", .{});
                            return null;
                        }
                        if (!appendExtent(fx, &rec, block_size, klog_arg)) return null;
                        if (rec.flags & flag_multi_extent == 0) return .{ .file = fx.* };
                    } else if (recordMatches(&rec, component, ns, rr_skip, klog_arg)) {
                        if (rec.flags & flag_dir != 0) {
                            if (rec.flags & flag_multi_extent != 0) {
                                if (klog_arg) |k| k.warn("live: iso9660: multi-extent directory unsupported", .{});
                                return null;
                            }
                            return .{ .dir = .{ .lba = rec.extent_lba, .size = rec.data_len } };
                        }
                        var fx = FileExtents{};
                        if (!appendExtent(&fx, &rec, block_size, klog_arg)) return null;
                        if (rec.flags & flag_multi_extent == 0) return .{ .file = fx };
                        collecting = fx;
                    }
                    off += rec.rec_len;
                },
            }
        }
    }

    if (collecting != null) {
        if (klog_arg) |k| k.warn("live: iso9660: multi-extent file truncated at directory end", .{});
    }
    return null;
}

fn rootRrSkip(src: Source, block_size: u32, root: DirLoc) u8 {
    var buf: [logical_sector]u8 = undefined;
    const chunk: usize = @intCast(@min(@as(u64, logical_sector), @as(u64, root.size)));
    if (chunk == 0) return 0;
    if (!readFull(src, @as(u64, root.lba) * @as(u64, block_size), buf[0..chunk])) return 0;
    switch (parseDirRecord(buf[0..chunk], 0)) {
        .rec => |r| return rrSkipLen(r.su),
        else => return 0,
    }
}

fn walkPath(
    src: Source,
    block_size: u32,
    root: DirLoc,
    path: []const u8,
    ns: Namespace,
    klog_arg: ?kmsg,
) ?FileExtents {
    if (path.len < 2 or path[0] != '/') {
        if (klog_arg) |k| k.warn("live: iso9660: path must be absolute", .{});
        return null;
    }

    const rr_skip: u8 = if (ns == .primary) rootRrSkip(src, block_size, root) else 0;

    var cur = root;
    var depth: usize = 0;
    var it = std.mem.tokenizeScalar(u8, path, '/');
    while (it.next()) |component| {
        depth += 1;
        if (depth > max_path_depth) {
            if (klog_arg) |k| k.warn("live: iso9660: path deeper than {d} components", .{max_path_depth});
            return null;
        }
        const found = findInDirectory(src, block_size, cur, component, ns, rr_skip, klog_arg) orelse return null;
        const is_last = it.peek() == null;
        switch (found) {
            .dir => |d| {
                if (is_last) return null; // path names a directory, not a file
                cur = d;
            },
            .file => |fx| {
                if (!is_last) return null; // file in the middle of the path
                if (fx.total_size == 0) return null;
                return fx;
            },
        }
    }
    return null;
}

// --- Tests ---

const testing = std.testing;

fn wBoth32(buf: []u8, off: usize, v: u32) void {
    std.mem.writeInt(u32, buf[off..][0..4], v, .little);
    std.mem.writeInt(u32, buf[off + 4 ..][0..4], v, .big);
}

/// Write a directory record; returns the offset just past it.
fn putDirRecord(buf: []u8, off: usize, lba: u32, size: u32, flags: u8, name: []const u8, su: []const u8) usize {
    const pad: usize = 1 - (name.len & 1);
    const rec_len = 33 + name.len + pad + su.len;
    buf[off] = @intCast(rec_len);
    wBoth32(buf, off + 2, lba);
    wBoth32(buf, off + 10, size);
    buf[off + 25] = flags;
    buf[off + 28] = 1; // volume sequence number (both-endian u16 pair)
    buf[off + 31] = 1;
    buf[off + 32] = @intCast(name.len);
    @memcpy(buf[off + 33 ..][0..name.len], name);
    @memcpy(buf[off + 33 + name.len + pad ..][0..su.len], su);
    return off + rec_len;
}

fn putVd(img: []u8, sector: usize, vd_type: u8) []u8 {
    const s = img[sector * logical_sector ..][0..logical_sector];
    s[0] = vd_type;
    @memcpy(s[1..6], "CD001");
    s[6] = 1;
    return s;
}

fn putVdRoot(s: []u8, root_lba: u32, root_size: u32) void {
    std.mem.writeInt(u16, s[128..130], 2048, .little);
    std.mem.writeInt(u16, s[130..132], 2048, .big);
    _ = putDirRecord(s, 156, root_lba, root_size, flag_dir, &[_]u8{0x00}, &[_]u8{});
}

const test_sectors = 32;
const TestIso = [test_sectors * logical_sector]u8;

const sp_entry = [_]u8{ 'S', 'P', 7, 1, 0xBE, 0xEF, 0 };
const nm_live = [_]u8{ 'N', 'M', 9, 1, 0 } ++ "live".*;
const nm_squash = [_]u8{ 'N', 'M', 18, 1, 0 } ++ "root.squashfs".*;

const file_lba: u32 = 30;
const file_size: u32 = 5000;

/// Layout: PVD@16, optional Joliet SVD@17, terminator@18;
/// primary root@20 -> LIVE@21 -> ROOT.SQUASHFS;1; joliet root@22 -> live@23 -> root.squashfs.
fn buildTestIso(img: *TestIso, opts: struct { joliet: bool = true, rr: bool = true }) void {
    @memset(img, 0);

    putVdRoot(putVd(img, 16, 1), 20, logical_sector);
    if (opts.joliet) {
        const svd = putVd(img, 17, 2);
        @memcpy(svd[88..91], "%/E");
        putVdRoot(svd, 22, logical_sector);
    }
    _ = putVd(img, 18, 255);

    const dot = [_]u8{0x00};
    const dotdot = [_]u8{0x01};
    const no_su = [_]u8{};

    // Primary root and /live
    const proot = img[20 * logical_sector ..][0..logical_sector];
    var off = putDirRecord(proot, 0, 20, logical_sector, flag_dir, &dot, if (opts.rr) &sp_entry else &no_su);
    off = putDirRecord(proot, off, 20, logical_sector, flag_dir, &dotdot, &no_su);
    _ = putDirRecord(proot, off, 21, logical_sector, flag_dir, "LIVE", if (opts.rr) &nm_live else &no_su);

    const plive = img[21 * logical_sector ..][0..logical_sector];
    off = putDirRecord(plive, 0, 21, logical_sector, flag_dir, &dot, &no_su);
    off = putDirRecord(plive, off, 20, logical_sector, flag_dir, &dotdot, &no_su);
    _ = putDirRecord(plive, off, file_lba, file_size, 0, "ROOT.SQUASHFS;1", if (opts.rr) &nm_squash else &no_su);

    if (opts.joliet) {
        const j_live = [_]u8{ 0, 'l', 0, 'i', 0, 'v', 0, 'e' };
        const j_squash = blk: {
            var b: [26]u8 = undefined;
            for ("root.squashfs", 0..) |c, i| {
                b[2 * i] = 0;
                b[2 * i + 1] = c;
            }
            break :blk b;
        };
        const jroot = img[22 * logical_sector ..][0..logical_sector];
        off = putDirRecord(jroot, 0, 22, logical_sector, flag_dir, &dot, &no_su);
        off = putDirRecord(jroot, off, 22, logical_sector, flag_dir, &dotdot, &no_su);
        _ = putDirRecord(jroot, off, 23, logical_sector, flag_dir, &j_live, &no_su);

        const jlive = img[23 * logical_sector ..][0..logical_sector];
        off = putDirRecord(jlive, 0, 23, logical_sector, flag_dir, &dot, &no_su);
        off = putDirRecord(jlive, off, 22, logical_sector, flag_dir, &dotdot, &no_su);
        _ = putDirRecord(jlive, off, file_lba, file_size, 0, &j_squash, &no_su);
    }
}

test "parseDirRecord valid record and system-use slicing" {
    var buf: [128]u8 = @splat(0);
    _ = putDirRecord(&buf, 0, 21, 4096, flag_dir, "LIVE", &nm_live);
    const r = parseDirRecord(&buf, 0).rec;
    try testing.expectEqual(@as(u32, 21), r.extent_lba);
    try testing.expectEqual(@as(u32, 4096), r.data_len);
    try testing.expectEqual(flag_dir, r.flags);
    try testing.expectEqualStrings("LIVE", r.name);
    // "LIVE" is even-length: one pad byte before the SU area
    try testing.expectEqualSlices(u8, &nm_live, r.su);

    // Odd-length name: no pad byte
    var buf2: [128]u8 = @splat(0);
    _ = putDirRecord(&buf2, 0, 30, 100, 0, "ABC", &sp_entry);
    const r2 = parseDirRecord(&buf2, 0).rec;
    try testing.expectEqualStrings("ABC", r2.name);
    try testing.expectEqualSlices(u8, &sp_entry, r2.su);
}

test "parseDirRecord padding and malformed" {
    var buf: [64]u8 = @splat(0);
    try testing.expect(parseDirRecord(&buf, 0) == .padding);

    buf[0] = 10; // rec_len < 34
    try testing.expect(parseDirRecord(&buf, 0) == .malformed);

    buf[0] = 200; // overruns the buffer
    try testing.expect(parseDirRecord(&buf, 0) == .malformed);

    var buf2: [64]u8 = @splat(0);
    buf2[0] = 40;
    buf2[32] = 20; // 33 + name_len > rec_len
    try testing.expect(parseDirRecord(&buf2, 0) == .malformed);
}

test "matchPlainName" {
    try testing.expect(matchPlainName("ROOT.SQUASHFS;1", "root.squashfs"));
    try testing.expect(matchPlainName("LIVE", "live"));
    try testing.expect(matchPlainName("README.;1", "readme")); // trailing dot from 8.3 mangling
    try testing.expect(!matchPlainName("OTHER.SQU;1", "root.squashfs"));
    try testing.expect(!matchPlainName(&[_]u8{0x00}, "root.squashfs"));
    try testing.expect(!matchPlainName(&[_]u8{0x01}, "root.squashfs"));
}

test "matchJolietName" {
    const good = [_]u8{ 0, 'l', 0, 'i', 0, 'v', 0, 'e' };
    try testing.expect(matchJolietName(&good, "live"));
    try testing.expect(!matchJolietName(&good, "liv"));
    try testing.expect(!matchJolietName(&good, "Live"));

    const versioned = good ++ [_]u8{ 0, ';', 0, '1' };
    try testing.expect(matchJolietName(&versioned, "live"));

    const non_ascii = [_]u8{ 0x30, 'l', 0, 'i', 0, 'v', 0, 'e' };
    try testing.expect(!matchJolietName(&non_ascii, "live"));
    try testing.expect(!matchJolietName(&[_]u8{0x00}, "live"));
}

test "rrName single and continued NM" {
    var out: [256]u8 = undefined;

    switch (rrName(&nm_squash, 0, &out)) {
        .name => |n| try testing.expectEqualStrings("root.squashfs", n),
        else => return error.TestUnexpectedResult,
    }

    // Two NM entries, first with CONTINUE flag
    const nm_a = [_]u8{ 'N', 'M', 9, 1, 1 } ++ "root".*;
    const nm_b = [_]u8{ 'N', 'M', 14, 1, 0 } ++ ".squashfs".*;
    const both = nm_a ++ nm_b;
    switch (rrName(&both, 0, &out)) {
        .name => |n| try testing.expectEqualStrings("root.squashfs", n),
        else => return error.TestUnexpectedResult,
    }

    // NM with CONTINUE but no follow-up inline: continues in a CE area
    switch (rrName(&nm_a, 0, &out)) {
        .incomplete => {},
        else => return error.TestUnexpectedResult,
    }

    // CE present without any inline NM: the name may live in the continuation area
    const ce = [_]u8{ 'C', 'E', 28, 1 } ++ [_]u8{0} ** 24;
    switch (rrName(&ce, 0, &out)) {
        .incomplete => {},
        else => return error.TestUnexpectedResult,
    }

    // No NM at all
    const tf = [_]u8{ 'T', 'F', 5, 1, 0 };
    try testing.expect(rrName(&tf, 0, &out) == .none);

    // skip consumes leading bytes (SP-declared skip area)
    const skipped = [_]u8{ 0xAA, 0xBB } ++ nm_squash;
    switch (rrName(&skipped, 2, &out)) {
        .name => |n| try testing.expectEqualStrings("root.squashfs", n),
        else => return error.TestUnexpectedResult,
    }
}

test "rrSkipLen" {
    try testing.expectEqual(@as(u8, 0), rrSkipLen(&nm_live));
    try testing.expectEqual(@as(u8, 0), rrSkipLen(&[_]u8{}));
    const sp_skip2 = [_]u8{ 'S', 'P', 7, 1, 0xBE, 0xEF, 2 };
    try testing.expectEqual(@as(u8, 2), rrSkipLen(&sp_skip2));
    const bad_check = [_]u8{ 'S', 'P', 7, 1, 0xAA, 0xEF, 2 };
    try testing.expectEqual(@as(u8, 0), rrSkipLen(&bad_check));
}

test "locate via Joliet" {
    var img: TestIso = undefined;
    buildTestIso(&img, .{});
    const fx = locateFileInSource(.{ .mem = &img }, "/live/root.squashfs", null) orelse
        return error.TestUnexpectedResult;
    try testing.expectEqual(@as(u8, 1), fx.count);
    try testing.expectEqual(@as(u64, file_lba) * 2048, fx.extents[0].start_byte);
    try testing.expectEqual(@as(u64, file_size), fx.extents[0].size);
    try testing.expectEqual(@as(u64, file_size), fx.total_size);
}

test "locate via Rock Ridge NM when no Joliet" {
    var img: TestIso = undefined;
    buildTestIso(&img, .{ .joliet = false });
    const fx = locateFileInSource(.{ .mem = &img }, "/live/root.squashfs", null) orelse
        return error.TestUnexpectedResult;
    try testing.expectEqual(@as(u64, file_lba) * 2048, fx.extents[0].start_byte);
}

test "locate via plain identifiers when no Joliet and no RR" {
    var img: TestIso = undefined;
    buildTestIso(&img, .{ .joliet = false, .rr = false });
    const fx = locateFileInSource(.{ .mem = &img }, "/live/root.squashfs", null) orelse
        return error.TestUnexpectedResult;
    try testing.expectEqual(@as(u64, file_lba) * 2048, fx.extents[0].start_byte);
    try testing.expectEqual(@as(u64, file_size), fx.total_size);
}

test "locate multi-extent file" {
    var img: TestIso = undefined;
    buildTestIso(&img, .{ .joliet = false, .rr = false });
    // Append a two-extent file to the primary root directory
    const proot = img[20 * logical_sector ..][0..logical_sector];
    var off: usize = 0;
    while (proot[off] != 0) off += proot[off];
    off = putDirRecord(proot, off, 40, 1000, flag_multi_extent, "BIG.BIN;1", &[_]u8{});
    _ = putDirRecord(proot, off, 45, 500, 0, "BIG.BIN;1", &[_]u8{});

    const fx = locateFileInSource(.{ .mem = &img }, "/big.bin", null) orelse
        return error.TestUnexpectedResult;
    try testing.expectEqual(@as(u8, 2), fx.count);
    try testing.expectEqual(@as(u64, 40 * 2048), fx.extents[0].start_byte);
    try testing.expectEqual(@as(u64, 1000), fx.extents[0].size);
    try testing.expectEqual(@as(u64, 45 * 2048), fx.extents[1].start_byte);
    try testing.expectEqual(@as(u64, 500), fx.extents[1].size);
    try testing.expectEqual(@as(u64, 1500), fx.total_size);
}

test "record found after mid-sector padding in a multi-sector directory" {
    var img: TestIso = undefined;
    buildTestIso(&img, .{ .joliet = false, .rr = false });
    // Grow the primary root directory to 2 sectors (20 and 21 are consecutive) by
    // rewriting the PVD's root record; sector 20 ends in padding, the target lives
    // at the start of sector 21.
    const pvd = img[16 * logical_sector ..][0..logical_sector];
    _ = putDirRecord(pvd, 156, 20, 2 * logical_sector, flag_dir, &[_]u8{0x00}, &[_]u8{});
    @memset(img[21 * logical_sector ..][0..logical_sector], 0);
    _ = putDirRecord(img[21 * logical_sector ..][0..logical_sector], 0, file_lba, file_size, 0, "TAIL.BIN;1", &[_]u8{});

    const fx = locateFileInSource(.{ .mem = &img }, "/tail.bin", null) orelse
        return error.TestUnexpectedResult;
    try testing.expectEqual(@as(u64, file_lba) * 2048, fx.extents[0].start_byte);
}

test "lookup failures return null" {
    var img: TestIso = undefined;
    buildTestIso(&img, .{});
    const src = Source{ .mem = &img };

    try testing.expect(locateFileInSource(src, "/live/missing.squashfs", null) == null);
    try testing.expect(locateFileInSource(src, "/live", null) == null); // directory, not a file
    try testing.expect(locateFileInSource(src, "/live/root.squashfs/x", null) == null); // file mid-path
    try testing.expect(locateFileInSource(src, "relative/path", null) == null);
    try testing.expect(locateFileInSource(src, "/a/b/c/d/e/f/g/h/i", null) == null); // too deep

    var zeros: TestIso = @splat(0);
    try testing.expect(locateFileInSource(.{ .mem = &zeros }, "/live/root.squashfs", null) == null); // no CD001

    var bad_bs: TestIso = undefined;
    buildTestIso(&bad_bs, .{ .joliet = false });
    std.mem.writeInt(u16, bad_bs[16 * logical_sector + 128 ..][0..2], 4096, .little);
    try testing.expect(locateFileInSource(.{ .mem = &bad_bs }, "/live/root.squashfs", null) == null);
}
