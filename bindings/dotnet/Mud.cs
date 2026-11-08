// mudRPC for .NET: one file over the C ABI through P/Invoke. No package, no dependencies. Copy it into your project
// (no unsafe code, so no project settings) and point MUDRPC_LIB at libmudrpc, or put the library next to your assembly.
//
// A call is a message; a reply may come hours later or expire; nothing blocks. You poll: drain Outgoing() into your
// transport and call Sent(), feed what arrives to Deliver(), call Tick(). `now` is your uptime in seconds.
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
#nullable enable

namespace MudRpc
{
    /// <summary>What a delivered wire was, or why it was dropped.</summary>
    public enum Kind { None = 0, Call = 1, Reply = 2, Event = 3, Expired = 4, Dropped = 5, Flood = 6, Rollback = 7 }

    /// <summary>Why a wire never reached your code. Log every one in development; count them in the field.</summary>
    public enum Dropped { None = 0, Version = 1, Open = 2, Decode = 3, Duplicate = 4, NoPending = 5, Topic = 6, Seq = 7, Id = 8, Replay = 9, Rate = 10 }

    /// <summary>Which key slot opened the last accepted wire.</summary>
    public enum Slot { Current = 0, Previous = 1, Next = 2 }

    /// <summary>A failure code from the library. <see cref="Code"/> is the C constant: -2 Full, -3 TooLarge, -4 Corrupt, -6 Epoch, -7 Unprovisioned, -8 Exhausted, -9 Rate, -10 Busy.</summary>
    public sealed class MudException : Exception
    {
        public int Code { get; }
        public bool IsFull => Code == -2;
        internal MudException(int code) : base("mudRPC: " + Name(code)) { Code = code; }
        static string Name(int c) => c switch { -1 => "invalid argument", -2 => "full", -3 => "too large", -4 => "corrupt", -5 => "bad memory", -6 => "epoch not higher", -7 => "unprovisioned (sender 0)", -8 => "counter exhausted", -9 => "this second's sends spent", -10 => "busy: another thread holds this peer", -99 => "panic", _ => c.ToString() };
    }

    /// <summary>One delivered wire. For a Call, pass it back to <see cref="Peer.Reply"/>. For Flood and Rollback, <see cref="Sender"/> is the number concerned.</summary>
    public sealed record Incoming(Kind Kind, byte[] Id, string Topic, string Method, string ReplyTo, string Detail, byte[] Bytes, uint TtlSecs, uint Seq, uint Status, bool End, Dropped Dropped, uint Sender);

    /// <summary>What the transport must do next: listen on a topic, or send a wire with its lifetime.</summary>
    public sealed record Outgoing(bool Listen, string Topic, byte[] Wire, uint TtlSecs);

    /// <summary>A scenario event from the simulator; clone, spam, stage, rotate, retire, warp, hostile, stale_reboot, torn_save, revoke and telemetry are the host's to perform.</summary>
    public sealed record SimEvent(ulong AtMs, string Kind, ulong A, ulong B, string Text);

    public static class Mud
    {
        /// <summary>gRPC's name for a status number, "FAILED_PRECONDITION" for 9, "" past 16.</summary>
        public static string StatusName(uint status) => Marshal.PtrToStringUTF8(Native.mud_status_name(status))!;
    }

    internal static class Native
    {
        const string Lib = "mudrpc";
        [StructLayout(LayoutKind.Sequential)] internal struct Buf { public IntPtr Ptr; public nuint Len; }
        [StructLayout(LayoutKind.Sequential)] internal struct Id { public uint A, B, C; }   /* 12 bytes, 4-aligned, as in C */
        [StructLayout(LayoutKind.Sequential)] internal struct Config { public Buf Name; public Buf Key; public uint Sender; public uint Epoch; public Buf Seed; public uint Budget; public uint Hub; public uint Cipher; }
        [StructLayout(LayoutKind.Sequential)] internal struct In { public int Kind; public Id Id; public Buf Topic, Method, ReplyTo, Detail, Bytes; public uint TtlSecs, Seq, Status; public int End, Dropped; public uint Sender; }
        [StructLayout(LayoutKind.Sequential)] internal struct Out { public int Kind; public Buf Topic, Wire; public uint TtlSecs; }
        [StructLayout(LayoutKind.Sequential)] internal struct Event { public ulong AtMs; public int Kind; public ulong A, B; public Buf Text; }

        static Native()
        {
            NativeLibrary.SetDllImportResolver(typeof(Native).Assembly, (name, asm, path) =>
            {
                if (name != Lib) return IntPtr.Zero;
                var file = OperatingSystem.IsWindows() ? "mudrpc.dll" : OperatingSystem.IsMacOS() ? "libmudrpc.dylib" : "libmudrpc.so";
                var env = Environment.GetEnvironmentVariable("MUDRPC_LIB");
                var at = string.IsNullOrEmpty(env) ? System.IO.Path.Combine(AppContext.BaseDirectory, file) : env;
                if (NativeLibrary.TryLoad(at, out var h)) return h;
                if (NativeLibrary.TryLoad(Lib, asm, null, out h)) return h;   // the runtime's own search: the NuGet package's runtimes/<rid>/native
                throw new DllNotFoundException("libmudrpc not found at " + at + ": build with `cargo build --release -p mudrpc-c`, then set MUDRPC_LIB or put it beside the assembly");
            });
        }

        [DllImport(Lib)] internal static extern nuint mud_size();
        [DllImport(Lib)] internal static extern nuint mud_hub_size();
        [DllImport(Lib)] internal static extern nuint mud_align();
        [DllImport(Lib)] internal static extern int mud_new(IntPtr mem, nuint len, ref Config config);
        [DllImport(Lib)] internal static extern int mud_save(IntPtr mem, nuint len, IntPtr @out, nuint outLen);
        [DllImport(Lib)] internal static extern int mud_resume(IntPtr mem, nuint len, uint epoch);
        [DllImport(Lib)] internal static extern int mud_renew(IntPtr mem, nuint len, uint epoch);
        [DllImport(Lib)] internal static extern int mud_drop(IntPtr mem, nuint len);
        [DllImport(Lib)] internal static extern int mud_roster(IntPtr mem, nuint len, uint sender, Buf key, Buf next);
        [DllImport(Lib)] internal static extern int mud_revoke(IntPtr mem, nuint len, uint sender);
        [DllImport(Lib)] internal static extern int mud_public_key(Buf seed, IntPtr output);
        [DllImport(Lib)] internal static extern int mud_stage(IntPtr mem, nuint len, Buf key);
        [DllImport(Lib)] internal static extern int mud_rotate(IntPtr mem, nuint len);
        [DllImport(Lib)] internal static extern int mud_retire(IntPtr mem, nuint len);
        [DllImport(Lib)] internal static extern int mud_opened(IntPtr mem, nuint len);
        [DllImport(Lib)] internal static extern int mud_call(IntPtr mem, nuint len, uint now, Buf service, Buf method, Buf req, uint ttl, out Id id);
        [DllImport(Lib)] internal static extern int mud_publish(IntPtr mem, nuint len, uint now, Buf topic, Buf payload, uint ttl);
        [DllImport(Lib)] internal static extern int mud_listen(IntPtr mem, nuint len, Buf topic);
        [DllImport(Lib)] internal static extern int mud_reply(IntPtr mem, nuint len, uint now, ref In call, uint seq, uint status, Buf detail, Buf payload, int end);
        [DllImport(Lib)] internal static extern int mud_deliver(IntPtr mem, nuint len, uint now, Buf topic, Buf wire, Buf scratch, out In incoming);
        [DllImport(Lib)] internal static extern int mud_tick(IntPtr mem, nuint len, uint now, out In incoming);
        [DllImport(Lib)] internal static extern int mud_outgoing(IntPtr mem, nuint len, uint now, Buf scratch, out Out outgoing);
        [DllImport(Lib)] internal static extern int mud_sent(IntPtr mem, nuint len);
        [DllImport(Lib)] internal static extern IntPtr mud_status_name(uint status);
        [DllImport(Lib)] internal static extern uint mud_pressure(IntPtr mem, nuint len);
        [DllImport(Lib)] internal static extern uint mud_count(IntPtr mem, nuint len, int which);
        [DllImport(Lib)] internal static extern uint mud_pending(IntPtr mem, nuint len);
        [DllImport(Lib)] internal static extern uint mud_queued(IntPtr mem, nuint len);
        [DllImport(Lib)] internal static extern int mud_internet();
        [DllImport(Lib)] internal static extern IntPtr mud_udp_new(Buf bind, Buf peers, out int code);
        [DllImport(Lib)] internal static extern void mud_udp_free(IntPtr udp);
        [DllImport(Lib)] internal static extern int mud_udp_port(IntPtr udp);
        [DllImport(Lib)] internal static extern int mud_udp_send(IntPtr udp, Buf topic, Buf wire);
        [DllImport(Lib)] internal static extern int mud_udp_listen(IntPtr udp, Buf topic);
        [DllImport(Lib)] internal static extern int mud_udp_poll(IntPtr udp, Buf scratch, out Buf topic, out Buf wire);
        [DllImport(Lib)] internal static extern IntPtr mud_sim_new(Buf scenario, out int line);
        [DllImport(Lib)] internal static extern void mud_sim_free(IntPtr sim);
        [DllImport(Lib)] internal static extern int mud_sim_peers(IntPtr sim);
        [DllImport(Lib)] internal static extern ulong mud_sim_last_event_ms(IntPtr sim);
        [DllImport(Lib)] internal static extern ulong mud_sim_now_ms(IntPtr sim);
        [DllImport(Lib)] internal static extern void mud_sim_step(IntPtr sim, ulong dt);
        [DllImport(Lib)] internal static extern void mud_sim_note(IntPtr sim, Buf text);
        [DllImport(Lib)] internal static extern int mud_sim_send(IntPtr sim, int radio, Buf topic, Buf wire, uint ttl);
        [DllImport(Lib)] internal static extern int mud_sim_listen(IntPtr sim, int radio, Buf topic);
        [DllImport(Lib)] internal static extern int mud_sim_poll(IntPtr sim, int radio, Buf scratch, out Buf topic, out Buf wire);
        [DllImport(Lib)] internal static extern void mud_sim_silent(IntPtr sim, int radio, int on);
        [DllImport(Lib)] internal static extern int mud_sim_log(IntPtr sim, Buf outBuf);
        [DllImport(Lib)] internal static extern int mud_sim_events(IntPtr sim);
        [DllImport(Lib)] internal static extern int mud_sim_event(IntPtr sim, int i, out Event e);

        internal static int Check(int code) => code < 0 ? throw new MudException(code) : code;
        internal static byte[] Copy(Buf b) { var a = new byte[(int)b.Len]; if (a.Length > 0) Marshal.Copy(b.Ptr, a, 0, a.Length); return a; }
        internal static string Text(Buf b) => Encoding.UTF8.GetString(Copy(b));

        /// <summary>A pinned copy of `bytes` for the duration of one call.</summary>
        internal sealed class Pin : IDisposable
        {
            readonly GCHandle h; readonly int len;
            public Pin(byte[] bytes) { h = GCHandle.Alloc(bytes, GCHandleType.Pinned); len = bytes.Length; }
            public Buf Buf => new Buf { Ptr = len == 0 ? IntPtr.Zero : h.AddrOfPinnedObject(), Len = (nuint)len };
            public IntPtr Ptr => h.AddrOfPinnedObject();
            public void Dispose() => h.Free();
        }
        internal static Pin PinText(string s) => new Pin(Encoding.UTF8.GetBytes(s));

        internal static byte[] Bytes(Id id) { var b = new byte[12]; BitConverter.GetBytes(id.A).CopyTo(b, 0); BitConverter.GetBytes(id.B).CopyTo(b, 4); BitConverter.GetBytes(id.C).CopyTo(b, 8); return b; }
        internal static Id ToId(byte[] b) => b.Length == 12 ? new Id { A = BitConverter.ToUInt32(b, 0), B = BitConverter.ToUInt32(b, 4), C = BitConverter.ToUInt32(b, 8) } : throw new MudException(-1);
        internal static readonly string[] EventKinds = { "", "partition", "heal", "jam", "unjam", "malformed", "replay", "flood", "bandwidth", "peer_down", "peer_up", "clone", "spam", "stage", "rotate", "retire", "warp", "hostile", "noise", "stale_reboot", "torn_save", "revoke", "telemetry", "hold" };

        internal static Incoming Wrap(In r) =>
            new Incoming((Kind)r.Kind, Bytes(r.Id), Text(r.Topic), Text(r.Method), Text(r.ReplyTo), Text(r.Detail), Copy(r.Bytes), r.TtlSecs, r.Seq, r.Status, r.End != 0, (Dropped)r.Dropped, r.Sender);
    }

    /// <summary>
    /// A mudRPC peer. <paramref name="name"/> is its inbox topic; <paramref name="key"/> the 32-byte fleet key;
    /// <paramref name="sender"/> the roster number (0 cannot send); <paramref name="epoch"/> a number the host persists
    /// beside the key and raises on every boot and restore, written to storage before the peer is created.
    /// A signed peer believes 16 senders; <paramref name="hub"/> is the hub shape, 256, for the host that believes a fleet;
    /// the line past either throws <see cref="MudException.IsFull"/>. <paramref name="cipher"/> is <see cref="CipherDefault"/> unless named.
    /// Not thread-safe: one thread drives a peer. <see cref="Save"/> bytes contain the key; store them as you store the key.
    /// </summary>
    public sealed class Peer : IDisposable
    {
        public const int MaxTopic = 64, Msg = 512;
        /// <summary>The ciphers, as <c>mudrpc.h</c> names them: AES the default, the NIST algorithm buyers ask for; ChaCha20-Poly1305 smaller and faster without an AES instruction.</summary>
        public const uint CipherChaCha20Poly1305 = 0, CipherAes256Gcm = 1, CipherDefault = CipherAes256Gcm;
        IntPtr mem;
        readonly int size;
        readonly byte[] scratch = new byte[MaxTopic + Msg], plain = new byte[Msg];

        /// <summary>Unsigned under the fleet key alone, or signed with this sender's 32-byte Ed25519 seed and a verification budget per second.</summary>
        public Peer(string name, byte[] key, uint sender, uint epoch, byte[]? seed = null, uint budget = 1000, bool hub = false, uint cipher = CipherDefault)
        {
            if (key.Length != 32) throw new ArgumentException("the key is 32 bytes", nameof(key));
            if (seed != null && seed.Length != 32) throw new ArgumentException("the seed is 32 bytes", nameof(seed));
            size = (int)(hub ? Native.mud_hub_size() : Native.mud_size());
            mem = Marshal.AllocHGlobal(size);
            using var n = Native.PinText(name); using var k = new Native.Pin(key); using var sd = new Native.Pin(seed ?? Array.Empty<byte>());
            var cfg = new Native.Config { Name = n.Buf, Key = k.Buf, Sender = sender, Epoch = epoch, Seed = sd.Buf, Budget = budget, Hub = hub ? 1u : 0u, Cipher = cipher };
            try { Native.Check(Native.mud_new(mem, (nuint)size, ref cfg)); } catch { Marshal.FreeHGlobal(mem); mem = IntPtr.Zero; throw; }
        }

        Peer(IntPtr restored, int size) { mem = restored; this.size = size; }

        /// <summary>A peer from <see cref="Save"/> bytes of this same build of the library, with a higher epoch from the host.</summary>
        public static Peer Restore(byte[] saved, uint epoch)
        {
            if (saved.Length != (int)Native.mud_size() && saved.Length != (int)Native.mud_hub_size()) throw new MudException(-4);
            var mem = Marshal.AllocHGlobal(saved.Length);
            Marshal.Copy(saved, 0, mem, saved.Length);
            try { Native.Check(Native.mud_resume(mem, (nuint)saved.Length, epoch)); } catch { Marshal.FreeHGlobal(mem); throw; }
            return new Peer(mem, saved.Length);
        }

        /// <summary>The peer's bytes, to write to flash. They contain the key, and cannot send until Restore has resumed them.</summary>
        public byte[] Save() { var b = new byte[size]; var o = Marshal.AllocHGlobal(b.Length); try { Native.Check(Native.mud_save(mem, (nuint)size, o, (nuint)size)); Marshal.Copy(o, b, 0, b.Length); } finally { Marshal.FreeHGlobal(o); } return b; }
        /// <summary>A higher epoch for a live peer; clears "counter exhausted".</summary>
        public void Renew(uint epoch) => Native.Check(Native.mud_renew(mem, (nuint)size, epoch));
        /// <summary>The next fleet key, before the flag day.</summary>
        public void Stage(byte[] key) { if (key.Length != 32) throw new ArgumentException("the key is 32 bytes", nameof(key)); using var k = new Native.Pin(key); Native.Check(Native.mud_stage(mem, (nuint)size, k.Buf)); }
        /// <summary>Previous ← current ← next.</summary>
        public void Rotate() => Native.Check(Native.mud_rotate(mem, (nuint)size));
        /// <summary>Drop the previous key; late wires under it are refused from now on.</summary>
        public void Retire() => Native.Check(Native.mud_retire(mem, (nuint)size));
        /// <summary>Which key slot opened the last accepted wire.</summary>
        public Slot Opened => (Slot)Native.Check(Native.mud_opened(mem, (nuint)size));
        /// <summary>Live table entries evicted, or senders refused a window, for want of room; rising means replay protection is degraded.</summary>
        public uint Pressure => Native.mud_pressure(mem, (nuint)size);
        /// <summary>Calls in flight, of 16: at 16 a call is "full".</summary>
        public uint Pending => Native.mud_pending(mem, (nuint)size);
        /// <summary>Items queued for the transport, of 16: at 16 every producer is "full".</summary>
        public uint Queued => Native.mud_queued(mem, (nuint)size);
        /// <summary>The operator's counters, kept across restores: managed (sealer taken for a change), rollbacks, floods, dropped.</summary>
        public (uint Managed, uint Rollbacks, uint Floods, uint Dropped) Counts => (Native.mud_count(mem, (nuint)size, 0), Native.mud_count(mem, (nuint)size, 1), Native.mud_count(mem, (nuint)size, 2), Native.mud_count(mem, (nuint)size, 3));

        /// <summary>Queues a call. Returns its id; the reply carries the same id, or <see cref="Tick"/> reports it expired.</summary>
        public byte[] Call(uint now, string service, string method, byte[] request, uint ttlSecs)
        {
            using var s = Native.PinText(service); using var m = Native.PinText(method); using var r = new Native.Pin(request);
            Native.Check(Native.mud_call(mem, (nuint)size, now, s.Buf, m.Buf, r.Buf, ttlSecs, out var id));
            return Native.Bytes(id);
        }

        /// <summary>Queues an event on a topic; no reply.</summary>
        public void Publish(uint now, string topic, byte[] payload, uint ttlSecs)
        {
            using var t = Native.PinText(topic); using var p = new Native.Pin(payload);
            Native.Check(Native.mud_publish(mem, (nuint)size, now, t.Buf, p.Buf, ttlSecs));
        }

        /// <summary>Announce and subscribe, one act.</summary>
        public void Listen(string topic) { using var t = Native.PinText(topic); Native.Check(Native.mud_listen(mem, (nuint)size, t.Buf)); }

        /// <summary>Answers a Call exactly as <see cref="Deliver"/> returned it, status gRPC's number and detail its text. Stream: seq 0..n, end on the last.</summary>
        public void Reply(uint now, Incoming call, uint status, byte[] payload, bool end = true, uint seq = 0, string detail = "")
        {
            if (call.Kind != Kind.Call) throw new MudException(-1);
            using var t = Native.PinText(call.Topic); using var m = Native.PinText(call.Method); using var r = Native.PinText(call.ReplyTo); using var b = new Native.Pin(call.Bytes); using var p = new Native.Pin(payload); using var why = Native.PinText(detail);
            var c = new Native.In { Kind = 1, Id = Native.ToId(call.Id), Topic = t.Buf, Method = m.Buf, ReplyTo = r.Buf, Bytes = b.Buf, TtlSecs = call.TtlSecs, Seq = call.Seq, End = call.End ? 1 : 0 };
            Native.Check(Native.mud_reply(mem, (nuint)size, now, ref c, seq, status, why.Buf, p.Buf, end ? 1 : 0));
        }

        /// <summary>A wire from the transport. Every wire gets a verdict; a Dropped kind names why.</summary>
        public Incoming Deliver(uint now, string topic, byte[] wire)
        {
            using var t = Native.PinText(topic); using var w = new Native.Pin(wire); using var s = new Native.Pin(plain);
            Native.Check(Native.mud_deliver(mem, (nuint)size, now, t.Buf, w.Buf, s.Buf, out var r));
            return Native.Wrap(r);
        }

        /// <summary>Time passes: the next expired call, or null. Call until null.</summary>
        public Incoming? Tick(uint now) => Native.Check(Native.mud_tick(mem, (nuint)size, now, out var r)) == 0 ? null : Native.Wrap(r);

        /// <summary>Next item for the transport, left in place until <see cref="Sent"/>. Null when the outbox is empty.</summary>
        public Outgoing? Outgoing(uint now)
        {
            using var s = new Native.Pin(scratch);
            int kind = Native.Check(Native.mud_outgoing(mem, (nuint)size, now, s.Buf, out var o));
            return kind == 0 ? null : new Outgoing(kind == 2, Native.Text(o.Topic), Native.Copy(o.Wire), o.TtlSecs);
        }

        /// <summary>Signed peers: a roster line for <paramref name="sender"/>, its public key and the next one during a rotation.</summary>
        public void Roster(uint sender, byte[] key, byte[]? next = null) { using var k = new Native.Pin(key); using var nx = new Native.Pin(next ?? Array.Empty<byte>()); Native.Check(Native.mud_roster(mem, (nuint)size, sender, k.Buf, nx.Buf)); }
        public void Revoke(uint sender) => Native.Check(Native.mud_revoke(mem, (nuint)size, sender));
        /// <summary>The 32-byte public key of a seed, for rosters.</summary>
        public static byte[] PublicKey(byte[] seed) { var o = new byte[32]; using var s = new Native.Pin(seed); using var p = new Native.Pin(o); Native.Check(Native.mud_public_key(s.Buf, p.Ptr)); return o; }

        /// <summary>The transport took the item <see cref="Outgoing"/> showed.</summary>
        public void Sent() => Native.Check(Native.mud_sent(mem, (nuint)size));

        /// <summary>Ends the peer: its block is zeroed, keys included, then freed. Every later call throws.</summary>
        public void Dispose() { if (mem != IntPtr.Zero) { Native.mud_drop(mem, (nuint)size); Marshal.FreeHGlobal(mem); mem = IntPtr.Zero; } }
    }

    /// <summary>A product's UDP: each wire to every listed peer, private addresses alone unless the library was built with the cargo feature <c>internet</c>, which no release is.</summary>
    public sealed class Udp : IDisposable
    {
        IntPtr udp;
        readonly object held = new();   // a channel's pump and the owner's Dispose never meet inside the library
        readonly byte[] scratch = new byte[Peer.MaxTopic + Peer.Msg];

        /// <summary>Whether this library may bind or send to a public address.</summary>
        public static bool Internet => Native.mud_internet() == 1;

        /// <summary>Binds <c>"ip:port"</c> and sends to each peer's, at least one; a bad or public address, or no peer, is <c>MudException</c> -1 (<c>0.0.0.0</c> is public), a bind the system refuses -2.</summary>
        public Udp(string bind, params string[] peers)
        {
            using var b = Native.PinText(bind); using var p = Native.PinText(string.Join(' ', peers));
            udp = Native.mud_udp_new(b.Buf, p.Buf, out var code);
            if (udp == IntPtr.Zero) throw new MudException(code);
        }

        /// <summary>The bound port, the one chosen for port 0.</summary>
        public int Port { get { lock (held) return Native.Check(Native.mud_udp_port(Live)); } }

        IntPtr Live => udp != IntPtr.Zero ? udp : throw new ObjectDisposedException(nameof(Udp));

        /// <summary>One outgoing item: a listen keeps wires on its topic from then, every other topic dropped unread; a send throws when no peer took the wire, so the item stays queued.</summary>
        public void Send(Outgoing o)
        {
            using var t = Native.PinText(o.Topic); using var w = new Native.Pin(o.Wire);
            int c;
            lock (held) c = o.Listen ? Native.mud_udp_listen(Live, t.Buf) : Native.mud_udp_send(Live, t.Buf, w.Buf);
            if (c != 0) throw new MudException(c);
        }

        /// <summary>The next wire from a listed peer, or null.</summary>
        public (string Topic, byte[] Wire)? Poll()
        {
            lock (held)
            {
                using var s = new Native.Pin(scratch);
                return Native.Check(Native.mud_udp_poll(Live, s.Buf, out var topic, out var wire)) == 1 ? (Native.Text(topic), Native.Copy(wire)) : null;
            }
        }

        public void Dispose() { lock (held) { if (udp != IntPtr.Zero) { Native.mud_udp_free(udp); udp = IntPtr.Zero; } } }
    }

    /// <summary>The simulator: one world from scenario text, radios 0..Peers-1. A development tool.</summary>
    public sealed class Sim : IDisposable
    {
        IntPtr sim;
        readonly byte[] scratch = new byte[Peer.MaxTopic + Peer.Msg];
        public int Peers { get; }
        public ulong LastEventMs { get; }

        public Sim(string scenario)
        {
            using var t = Native.PinText(scenario);
            sim = Native.mud_sim_new(t.Buf, out var line);
            if (sim == IntPtr.Zero) throw new ArgumentException($"scenario error at line {line}");
            Peers = Native.mud_sim_peers(sim);
            LastEventMs = Native.mud_sim_last_event_ms(sim);
        }

        public ulong NowMs => Native.mud_sim_now_ms(sim);
        public void Step(ulong dtMs = 100) => Native.mud_sim_step(sim, dtMs);
        public void Note(string text) { using var t = Native.PinText(text); Native.mud_sim_note(sim, t.Buf); }
        public void Silent(int radio, bool on) => Native.mud_sim_silent(sim, radio, on ? 1 : 0);

        /// <summary>False when the radio is down; the item stays with the peer.</summary>
        public bool Send(int radio, string topic, byte[] wire, uint ttlSecs)
        {
            using var t = Native.PinText(topic); using var w = new Native.Pin(wire);
            int c = Native.mud_sim_send(sim, radio, t.Buf, w.Buf, ttlSecs);
            if (c == -1) throw new MudException(c);
            return c == 0;
        }

        public bool Listen(int radio, string topic)
        {
            using var t = Native.PinText(topic);
            int c = Native.mud_sim_listen(sim, radio, t.Buf);
            if (c == -1) throw new MudException(c);
            return c == 0;
        }

        /// <summary>Null when idle; otherwise (1, topic, wire) for a message, (2, "", empty) on-mesh, (3, "", empty) off-mesh.</summary>
        public (int Kind, string Topic, byte[] Wire)? Poll(int radio)
        {
            using var s = new Native.Pin(scratch);
            int k = Native.Check(Native.mud_sim_poll(sim, radio, s.Buf, out var topic, out var wire));
            return k == 0 ? null : (k, k == 1 ? Native.Text(topic) : "", k == 1 ? Native.Copy(wire) : Array.Empty<byte>());
        }

        public IReadOnlyList<SimEvent> Events()
        {
            var list = new List<SimEvent>();
            for (int i = 0; i < Native.mud_sim_events(sim); i++) { Native.Check(Native.mud_sim_event(sim, i, out var e)); list.Add(new SimEvent(e.AtMs, Native.EventKinds[e.Kind], e.A, e.B, Native.Text(e.Text))); }
            return list;
        }

        public string Log()
        {
            int need = Native.mud_sim_log(sim, default);
            var buf = new byte[need];
            using var p = new Native.Pin(buf);
            Native.mud_sim_log(sim, p.Buf);
            return Encoding.UTF8.GetString(buf);
        }

        public void Dispose() { if (sim != IntPtr.Zero) { Native.mud_sim_free(sim); sim = IntPtr.Zero; } }
    }
}
