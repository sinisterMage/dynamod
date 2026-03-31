/// IPC framing layer for dynamod-init.
/// Handles the wire protocol: [magic: 0x444D (2B)] [length: u32 LE (4B)] [payload (N B)]
/// Provides send/recv over Unix domain sockets with proper framing.
const std = @import("std");
const linux = std.os.linux;
const constants = @import("constants.zig");
const msgpack = @import("msgpack.zig");
const kmsg = @import("kmsg.zig");
const shutdown_mod = @import("shutdown.zig");

pub const ShutdownKind = shutdown_mod.ShutdownKind;

const HEADER_SIZE = 6; // 2 magic + 4 length

/// A received IPC message (payload only, header already stripped).
pub const ReceivedMessage = struct {
    payload: []const u8,
};

/// Message types from svmgr that init understands.
pub const InitMessage = union(enum) {
    heartbeat,
    request_shutdown: ShutdownKind,
    log_to_kmsg: struct { level: u8, message: []const u8 },
    unknown,
};

/// Read buffer for accumulating partial messages.
pub const ReadBuffer = struct {
    buf: [constants.max_message_size + HEADER_SIZE]u8 = undefined,
    len: usize = 0,

    /// Read available data from the fd into the buffer.
    /// Returns error.EndOfStream on EOF, error.WouldBlock if nothing available.
    pub fn readFrom(self: *ReadBuffer, fd: std.posix.fd_t) !void {
        const space = self.buf[self.len..];
        if (space.len == 0) {
            self.len = 0;
            return;
        }

        const n = std.posix.read(fd, space) catch |e| {
            if (e == error.WouldBlock) return;
            return e;
        };

        if (n == 0) return error.EndOfStream;
        self.len += n;
    }

    /// Extract and return the next complete message from the buffer.
    /// Returns null if no complete message is available yet.
    pub fn nextMessage(self: *ReadBuffer) ?InitMessage {
        while (self.len >= HEADER_SIZE) {
            if (self.buf[0] != constants.ipc_magic[0] or self.buf[1] != constants.ipc_magic[1]) {
                if (self.resync()) continue;
                self.len = 0;
                return null;
            }

            const payload_len = std.mem.readInt(u32, self.buf[2..6], .little);
            if (payload_len > constants.max_message_size) {
                self.len = 0;
                return null;
            }

            const total = HEADER_SIZE + payload_len;
            if (self.len < total) return null;

            const payload = self.buf[HEADER_SIZE..total];
            const msg = parseMessage(payload);

            if (self.len > total) {
                std.mem.copyForwards(u8, &self.buf, self.buf[total..self.len]);
            }
            self.len -= total;

            return msg;
        }
        return null;
    }

    fn resync(self: *ReadBuffer) bool {
        var i: usize = 1;
        while (i + 1 < self.len) : (i += 1) {
            if (self.buf[i] == constants.ipc_magic[0] and self.buf[i + 1] == constants.ipc_magic[1]) {
                std.mem.copyForwards(u8, &self.buf, self.buf[i..self.len]);
                self.len -= i;
                return true;
            }
        }
        return false;
    }
};

/// Parse a MessagePack payload into an InitMessage.
/// The payload is a MessagePack map produced by rmp-serde on the Rust side.
/// Structure: {"id": N, "kind": ..., "body": ...}
fn parseMessage(payload: []const u8) InitMessage {
    // The Rust side serializes Message as a msgpack map with fields:
    //   "id" -> uint
    //   "kind" -> variant
    //   "body" -> variant
    // rmp-serde serializes enums as {"variant_name": {fields...}} or just "variant_name"

    // Look for the "body" field in the top-level map
    const body_raw = msgpack.lookupMapString(payload, "body") orelse return .unknown;

    // The body is a msgpack value. For simple variants like Heartbeat,
    // rmp-serde encodes them as a string "Heartbeat" or a map {"Heartbeat": nil}.
    // Let's try to decode it.
    const body_result = msgpack.decode(body_raw) catch return .unknown;

    switch (body_result.value) {
        .string => |s| {
            if (std.mem.eql(u8, s, "Heartbeat")) return .heartbeat;
            if (std.mem.eql(u8, s, "HeartbeatAck")) return .heartbeat;
            if (std.mem.eql(u8, s, "Ack")) return .unknown;
            return .unknown;
        },
        else => {
            // Could be a map like {"RequestShutdown": {"kind": "Poweroff"}}
            // or {"LogToKmsg": {"level": 6, "message": "..."}}
            // For now, try to look up known keys
            if (msgpack.lookupMapString(body_raw, "RequestShutdown")) |shutdown_raw| {
                return parseShutdownRequest(shutdown_raw);
            }
            if (msgpack.lookupMapString(body_raw, "LogToKmsg")) |log_raw| {
                return parseLogToKmsg(log_raw);
            }
            return .unknown;
        },
    }
}

fn parseShutdownRequest(raw: []const u8) InitMessage {
    const kind_raw = msgpack.lookupMapString(raw, "kind") orelse return .unknown;
    const kind_result = msgpack.decode(kind_raw) catch return .unknown;
    if (kind_result.value != .string) return .unknown;

    const kind_str = kind_result.value.string;
    if (std.mem.eql(u8, kind_str, "Poweroff")) return .{ .request_shutdown = .poweroff };
    if (std.mem.eql(u8, kind_str, "Reboot")) return .{ .request_shutdown = .reboot };
    if (std.mem.eql(u8, kind_str, "Halt")) return .{ .request_shutdown = .halt };
    return .unknown;
}

fn parseLogToKmsg(raw: []const u8) InitMessage {
    const level_raw = msgpack.lookupMapString(raw, "level") orelse return .unknown;
    const level_result = msgpack.decode(level_raw) catch return .unknown;
    if (level_result.value != .uint) return .unknown;

    const msg_raw = msgpack.lookupMapString(raw, "message") orelse return .unknown;
    const msg_result = msgpack.decode(msg_raw) catch return .unknown;
    if (msg_result.value != .string) return .unknown;

    return .{ .log_to_kmsg = .{
        .level = @intCast(level_result.value.uint),
        .message = msg_result.value.string,
    } };
}

/// Encode and send a simple message (heartbeat ack, shutdown signal, etc.)
/// Uses a fixed buffer — no allocation.
pub fn sendHeartbeatAck(fd: std.posix.fd_t, msg_id: u64) void {
    var buf: [256]u8 = undefined;
    var offset: usize = HEADER_SIZE; // Leave room for header

    // Encode: {"id": msg_id, "kind": {"Response": {"in_reply_to": msg_id}}, "body": "HeartbeatAck"}
    offset += msgpack.encodeMapHeader(buf[offset..], 3) catch return;
    offset += msgpack.encodeString(buf[offset..], "id") catch return;
    offset += msgpack.encodeUint(buf[offset..], msg_id) catch return;
    offset += msgpack.encodeString(buf[offset..], "kind") catch return;
    // kind: {"Response": {"in_reply_to": N}}
    offset += msgpack.encodeMapHeader(buf[offset..], 1) catch return;
    offset += msgpack.encodeString(buf[offset..], "Response") catch return;
    offset += msgpack.encodeMapHeader(buf[offset..], 1) catch return;
    offset += msgpack.encodeString(buf[offset..], "in_reply_to") catch return;
    offset += msgpack.encodeUint(buf[offset..], msg_id) catch return;
    offset += msgpack.encodeString(buf[offset..], "body") catch return;
    offset += msgpack.encodeString(buf[offset..], "HeartbeatAck") catch return;

    const payload_len = offset - HEADER_SIZE;

    // Write header
    buf[0] = constants.ipc_magic[0];
    buf[1] = constants.ipc_magic[1];
    std.mem.writeInt(u32, buf[2..6], @intCast(payload_len), .little);

    _ = std.posix.write(fd, buf[0..offset]) catch {};
}

/// Send a shutdown signal notification to svmgr.
pub fn sendShutdownSignal(fd: std.posix.fd_t, sig_name: []const u8) void {
    var buf: [256]u8 = undefined;
    var offset: usize = HEADER_SIZE;

    // {"id": 0, "kind": "Event", "body": {"ShutdownSignal": {"signal": sig_name}}}
    offset += msgpack.encodeMapHeader(buf[offset..], 3) catch return;
    offset += msgpack.encodeString(buf[offset..], "id") catch return;
    offset += msgpack.encodeUint(buf[offset..], 0) catch return;
    offset += msgpack.encodeString(buf[offset..], "kind") catch return;
    offset += msgpack.encodeString(buf[offset..], "Event") catch return;
    offset += msgpack.encodeString(buf[offset..], "body") catch return;
    offset += msgpack.encodeMapHeader(buf[offset..], 1) catch return;
    offset += msgpack.encodeString(buf[offset..], "ShutdownSignal") catch return;
    offset += msgpack.encodeMapHeader(buf[offset..], 1) catch return;
    offset += msgpack.encodeString(buf[offset..], "signal") catch return;
    offset += msgpack.encodeString(buf[offset..], sig_name) catch return;

    const payload_len = offset - HEADER_SIZE;
    buf[0] = constants.ipc_magic[0];
    buf[1] = constants.ipc_magic[1];
    std.mem.writeInt(u32, buf[2..6], @intCast(payload_len), .little);

    _ = std.posix.write(fd, buf[0..offset]) catch {};
}
