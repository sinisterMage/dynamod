/// Minimal MessagePack encoder/decoder for dynamod-init.
/// Only supports the subset needed for init<->svmgr IPC:
/// - Maps (fixmap, map16)
/// - Strings (fixstr, str8, str16)
/// - Positive integers (fixint, uint8, uint16, uint32, uint64)
/// - Booleans
/// - Nil
///
/// This avoids pulling in a full MessagePack library for a ~200-line PID 1.
const std = @import("std");

pub const Value = union(enum) {
    nil,
    boolean: bool,
    uint: u64,
    int: i64,
    string: []const u8,
    map: []const MapEntry,
    array: []const Value,
};

pub const MapEntry = struct {
    key: Value,
    value: Value,
};

pub const DecodeError = error{
    UnexpectedEof,
    UnsupportedType,
    InvalidUtf8,
    BufferTooSmall,
};

pub const DecodeResult = struct {
    value: Value,
    consumed: usize,
};

/// Decode a single MessagePack value from the buffer.
/// Returns the decoded value and the number of bytes consumed.
/// String and map data point into the original buffer (zero-copy).
pub fn decode(buf: []const u8) DecodeError!DecodeResult {
    if (buf.len == 0) return DecodeError.UnexpectedEof;

    const tag = buf[0];

    // Positive fixint: 0x00 - 0x7f
    if (tag <= 0x7f) {
        return .{ .value = .{ .uint = tag }, .consumed = 1 };
    }

    // Negative fixint: 0xe0 - 0xff
    if (tag >= 0xe0) {
        const val: i8 = @bitCast(tag);
        return .{ .value = .{ .int = val }, .consumed = 1 };
    }

    // Fixmap: 0x80 - 0x8f
    if (tag >= 0x80 and tag <= 0x8f) {
        const count = tag & 0x0f;
        return decodeMap(buf[1..], count, 1);
    }

    // Fixarray: 0x90 - 0x9f
    if (tag >= 0x90 and tag <= 0x9f) {
        const count = tag & 0x0f;
        return decodeArray(buf[1..], count, 1);
    }

    // Fixstr: 0xa0 - 0xbf
    if (tag >= 0xa0 and tag <= 0xbf) {
        const len: usize = tag & 0x1f;
        if (buf.len < 1 + len) return DecodeError.UnexpectedEof;
        return .{ .value = .{ .string = buf[1 .. 1 + len] }, .consumed = 1 + len };
    }

    return switch (tag) {
        0xc0 => .{ .value = .nil, .consumed = 1 }, // nil
        0xc2 => .{ .value = .{ .boolean = false }, .consumed = 1 }, // false
        0xc3 => .{ .value = .{ .boolean = true }, .consumed = 1 }, // true
        0xcc => decodeUint(buf, 1), // uint8
        0xcd => decodeUint(buf, 2), // uint16
        0xce => decodeUint(buf, 4), // uint32
        0xcf => decodeUint(buf, 8), // uint64
        0xd0 => decodeInt(buf, 1), // int8
        0xd1 => decodeInt(buf, 2), // int16
        0xd2 => decodeInt(buf, 4), // int32
        0xd3 => decodeInt(buf, 8), // int64
        0xd9 => decodeStr(buf, 1), // str8
        0xda => decodeStr(buf, 2), // str16
        0xdc => blk: { // array16
            if (buf.len < 3) break :blk DecodeError.UnexpectedEof;
            const count = std.mem.readInt(u16, buf[1..3], .big);
            break :blk decodeArray(buf[3..], count, 3);
        },
        0xde => blk: { // map16
            if (buf.len < 3) break :blk DecodeError.UnexpectedEof;
            const count = std.mem.readInt(u16, buf[1..3], .big);
            break :blk decodeMap(buf[3..], count, 3);
        },
        else => DecodeError.UnsupportedType,
    };
}

fn decodeUint(buf: []const u8, comptime byte_count: comptime_int) DecodeError!DecodeResult {
    if (buf.len < 1 + byte_count) return DecodeError.UnexpectedEof;
    const IntType = std.meta.Int(.unsigned, byte_count * 8);
    const val = std.mem.readInt(IntType, buf[1..][0..byte_count], .big);
    return .{ .value = .{ .uint = val }, .consumed = 1 + byte_count };
}

fn decodeInt(buf: []const u8, comptime byte_count: comptime_int) DecodeError!DecodeResult {
    if (buf.len < 1 + byte_count) return DecodeError.UnexpectedEof;
    const IntType = std.meta.Int(.signed, byte_count * 8);
    const val = std.mem.readInt(IntType, buf[1..][0..byte_count], .big);
    return .{ .value = .{ .int = val }, .consumed = 1 + byte_count };
}

fn decodeStr(buf: []const u8, comptime len_bytes: comptime_int) DecodeError!DecodeResult {
    if (buf.len < 1 + len_bytes) return DecodeError.UnexpectedEof;
    const LenType = std.meta.Int(.unsigned, len_bytes * 8);
    const str_len: usize = std.mem.readInt(LenType, buf[1..][0..len_bytes], .big);
    const header = 1 + len_bytes;
    if (buf.len < header + str_len) return DecodeError.UnexpectedEof;
    return .{ .value = .{ .string = buf[header .. header + str_len] }, .consumed = header + str_len };
}

fn decodeMap(buf: []const u8, count: usize, header_size: usize) DecodeError!DecodeResult {
    // For the init side, we don't actually need to build a map structure.
    // We just skip over the entries to compute the total consumed bytes.
    // The caller will re-decode specific fields as needed.
    var offset: usize = 0;
    var i: usize = 0;
    while (i < count) : (i += 1) {
        // Decode key
        const key_result = try decode(buf[offset..]);
        offset += key_result.consumed;
        // Decode value
        const val_result = try decode(buf[offset..]);
        offset += val_result.consumed;
    }
    // We return nil for now; the actual map traversal is done via lookupString
    return .{ .value = .nil, .consumed = header_size + offset };
}

fn decodeArray(buf: []const u8, count: usize, header_size: usize) DecodeError!DecodeResult {
    var offset: usize = 0;
    var i: usize = 0;
    while (i < count) : (i += 1) {
        const result = try decode(buf[offset..]);
        offset += result.consumed;
    }
    return .{ .value = .nil, .consumed = header_size + offset };
}

/// Look up a string key in a MessagePack map and return the raw bytes of its value.
/// The buffer should start at the first byte of the map (including the tag).
/// Returns the slice of buf containing the value's raw msgpack bytes, or null.
pub fn lookupMapString(buf: []const u8, key: []const u8) ?[]const u8 {
    if (buf.len == 0) return null;
    const tag = buf[0];

    var count: usize = 0;
    var data_start: usize = 0;

    if (tag >= 0x80 and tag <= 0x8f) {
        count = tag & 0x0f;
        data_start = 1;
    } else if (tag == 0xde) {
        if (buf.len < 3) return null;
        count = std.mem.readInt(u16, buf[1..3], .big);
        data_start = 3;
    } else {
        return null;
    }

    var offset = data_start;
    var i: usize = 0;
    while (i < count) : (i += 1) {
        // Decode the key
        const key_result = decode(buf[offset..]) catch return null;
        const key_end = offset + key_result.consumed;

        // Check if this key matches
        if (key_result.value == .string and std.mem.eql(u8, key_result.value.string, key)) {
            // Return the raw bytes of the value
            const val_result = decode(buf[key_end..]) catch return null;
            return buf[key_end .. key_end + val_result.consumed];
        }

        // Skip the value
        offset = key_end;
        const val_result = decode(buf[offset..]) catch return null;
        offset += val_result.consumed;
    }
    return null;
}

// === Encoder ===

pub const EncodeError = error{
    BufferTooSmall,
};

/// Encode a nil value.
pub fn encodeNil(out: []u8) EncodeError!usize {
    if (out.len < 1) return EncodeError.BufferTooSmall;
    out[0] = 0xc0;
    return 1;
}

/// Encode a boolean.
pub fn encodeBool(out: []u8, val: bool) EncodeError!usize {
    if (out.len < 1) return EncodeError.BufferTooSmall;
    out[0] = if (val) 0xc3 else 0xc2;
    return 1;
}

/// Encode an unsigned integer using the smallest representation.
pub fn encodeUint(out: []u8, val: u64) EncodeError!usize {
    if (val <= 0x7f) {
        if (out.len < 1) return EncodeError.BufferTooSmall;
        out[0] = @intCast(val);
        return 1;
    } else if (val <= 0xff) {
        if (out.len < 2) return EncodeError.BufferTooSmall;
        out[0] = 0xcc;
        out[1] = @intCast(val);
        return 2;
    } else if (val <= 0xffff) {
        if (out.len < 3) return EncodeError.BufferTooSmall;
        out[0] = 0xcd;
        std.mem.writeInt(u16, out[1..3], @intCast(val), .big);
        return 3;
    } else if (val <= 0xffffffff) {
        if (out.len < 5) return EncodeError.BufferTooSmall;
        out[0] = 0xce;
        std.mem.writeInt(u32, out[1..5], @intCast(val), .big);
        return 5;
    } else {
        if (out.len < 9) return EncodeError.BufferTooSmall;
        out[0] = 0xcf;
        std.mem.writeInt(u64, out[1..9], val, .big);
        return 9;
    }
}

/// Encode a string.
pub fn encodeString(out: []u8, str: []const u8) EncodeError!usize {
    if (str.len <= 31) {
        const total = 1 + str.len;
        if (out.len < total) return EncodeError.BufferTooSmall;
        out[0] = 0xa0 | @as(u8, @intCast(str.len));
        @memcpy(out[1..][0..str.len], str);
        return total;
    } else if (str.len <= 0xff) {
        const total = 2 + str.len;
        if (out.len < total) return EncodeError.BufferTooSmall;
        out[0] = 0xd9;
        out[1] = @intCast(str.len);
        @memcpy(out[2..][0..str.len], str);
        return total;
    } else {
        const total = 3 + str.len;
        if (out.len < total) return EncodeError.BufferTooSmall;
        out[0] = 0xda;
        std.mem.writeInt(u16, out[1..3], @intCast(str.len), .big);
        @memcpy(out[3..][0..str.len], str);
        return total;
    }
}

/// Encode a fixmap header (up to 15 entries).
pub fn encodeMapHeader(out: []u8, count: u8) EncodeError!usize {
    if (count > 15) return EncodeError.BufferTooSmall;
    if (out.len < 1) return EncodeError.BufferTooSmall;
    out[0] = 0x80 | count;
    return 1;
}

test "decode positive fixint" {
    const result = try decode(&[_]u8{42});
    try std.testing.expectEqual(@as(u64, 42), result.value.uint);
    try std.testing.expectEqual(@as(usize, 1), result.consumed);
}

test "decode fixstr" {
    const buf = [_]u8{ 0xa5, 'h', 'e', 'l', 'l', 'o' };
    const result = try decode(&buf);
    try std.testing.expectEqualStrings("hello", result.value.string);
    try std.testing.expectEqual(@as(usize, 6), result.consumed);
}

test "decode bool" {
    const t = try decode(&[_]u8{0xc3});
    try std.testing.expect(t.value.boolean == true);
    const f = try decode(&[_]u8{0xc2});
    try std.testing.expect(f.value.boolean == false);
}

test "encode/decode uint roundtrip" {
    var buf: [16]u8 = undefined;
    const len = try encodeUint(&buf, 12345);
    const result = try decode(buf[0..len]);
    try std.testing.expectEqual(@as(u64, 12345), result.value.uint);
}

test "encode/decode string roundtrip" {
    var buf: [64]u8 = undefined;
    const len = try encodeString(&buf, "hello world");
    const result = try decode(buf[0..len]);
    try std.testing.expectEqualStrings("hello world", result.value.string);
}
