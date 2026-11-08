// The standard harness in C#: must produce the same log as adapters/harness.rs, byte for byte.
// usage: harness <scenario file> [--signed] [--gcm]
using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using MudRpc;

static class Program
{
    static readonly string[] Open = { "", "Short", "Tag", "Full", "Sig", "Roster", "Budget" };
    static readonly string[] Decode = { "", "Codec(Truncated)", "Codec(Overflow)", "Codec(Tag)", "Codec(Full)", "Codec(Utf8)", "Missing", "Id" };
    static byte[] Counting(int start) => Enumerable.Range(0, 32).Select(i => (byte)(start + i)).ToArray();   // as every harness derives them; a constant secret is refused
    static readonly byte[] Key = Counting(0), NextKey = Counting(32);

    static string DroppedName(Incoming inc) => inc.Dropped switch
    {
        Dropped.Version => $"Version({inc.Status})",
        Dropped.Open => inc.Status is >= 1 and <= 6 ? $"Open({Open[inc.Status]})" : "?",
        Dropped.Decode => $"Decode({(inc.Status < 8 ? Decode[inc.Status] : "?")})",
        Dropped.Duplicate => "Duplicate", Dropped.NoPending => "NoPending", Dropped.Topic => "Topic", Dropped.Seq => "Seq",
        Dropped.Id => "Id", Dropped.Replay => "Replay", Dropped.Rate => "Rate", _ => "?",
    };

    // Rust's {:?}: quotes and backslash escaped, \t \r \n \0 by name, other ASCII controls as \u{xx}
    static string Q(string s)
    {
        var o = new StringBuilder("\"");
        foreach (var ch in s)
        {
            switch (ch)
            {
                case '"': o.Append("\\\""); break; case '\\': o.Append("\\\\"); break; case '\n': o.Append("\\n"); break; case '\r': o.Append("\\r"); break;
                case '\t': o.Append("\\t"); break; case '\0': o.Append("\\0"); break;
                default: if (ch < 0x20 || ch == 0x7f) o.Append($"\\u{{{(int)ch:x}}}"); else o.Append(ch); break;
            }
        }
        return o.Append('"').ToString();
    }
    static string Q(byte[] b) => Q(Encoding.UTF8.GetString(b));

    // rpc::Error's name if the peer refuses a call, publish or reply, as the Rust harness logs it; null if it took it
    static string? Refused(Action a)
    {
        try { a(); return null; }
        catch (MudException e) { return e.Code switch { -2 => "Full", -3 => "TooLarge", -4 => "Corrupt", -6 => "Epoch", -7 => "Unprovisioned", -8 => "Exhausted", -9 => "Rate", _ => "?" }; }
    }

    static bool Signed, Gcm; static int Peers;
    static Peer NewPeer(string name, uint sender, uint epoch, bool listenNav)
    {
        var p = new Peer(name, Key, sender, epoch, Signed ? Counting((int)sender) : null, 1000, Signed && sender == 1, Gcm ? 1u : 0u);   // gcs is the hub
        if (Signed) for (uint s = 1; s <= Peers; s++) { try { p.Roster(s, Peer.PublicKey(Counting((int)s))); } catch (MudException e) when (e.IsFull) { break; } }   // a leaf believes the hub and the drones that fit
        if (listenNav) p.Listen("nav");
        return p;
    }

    static int Main(string[] args)
    {
        using var sim = new Sim(System.IO.File.ReadAllText(args[0], Encoding.UTF8));
        int n = sim.Peers;
        Signed = args.Skip(1).Contains("--signed"); Gcm = args.Skip(1).Contains("--gcm"); Peers = n;
        var names = Enumerable.Range(0, n).Select(i => i == 0 ? "gcs" : $"drone-{i}").ToArray();
        var hostKinds = new HashSet<string> { "clone", "spam", "stage", "rotate", "retire", "warp", "hostile", "stale_reboot", "torn_save", "revoke", "telemetry", "flood" };
        var host = new Queue<SimEvent>(sim.Events().Where(e => hostKinds.Contains(e.Kind)).OrderBy(e => e.AtMs));
        var clones = new HashSet<int>(host.Where(e => e.Kind == "clone").Select(e => (int)e.B));
        var peers = Enumerable.Range(0, n).Select(i => clones.Contains(i) ? null : NewPeer(names[i], (uint)i + 1, 2, i > 0)).ToArray();
        var images = peers.Select(p => p?.Save()).ToArray();   // what each host wrote to flash at boot
        var spam = new Dictionary<int, uint[]>();   // peer -> [rate, clock]
        var (flooding, landed) = (new bool[n], new int[n]);   // radios flooding; events each peer took this second
        ulong end = sim.LastEventMs + 50_000, t = 0;
        int ping = 2;
        ulong round = 0;   // gcs's cadence counts rounds, so a warp off the round grid keeps it
        while (t <= end)
        {
            uint now = (uint)(t / 1000);
            bool warped = false;
            if (round % 10 == 0)
                for (int i = 0; i < n; i++) { if (landed[i] > 0 && flooding.Any(f => f)) sim.Note($"peer{i} honest events landed this second: {landed[i]}"); landed[i] = 0; }
            while (host.Count > 0 && host.Peek().AtMs <= t)
            {
                var e = host.Dequeue();
                int a = (int)e.A, b = (int)e.B;
                switch (e.Kind)
                {
                    case "clone": peers[b] = NewPeer(names[a], (uint)a + 1, 1, true); sim.Note($"host: radio {b} is a clone of peer{a} from an old image"); break;
                    case "spam": spam.Remove(a); if (b > 0) spam[a] = new[] { (uint)b, 1_000_000u }; sim.Note($"host: peer{a} spams at {b}/s"); break;
                    case "stage": foreach (var p in peers) p?.Stage(NextKey); sim.Note("host: every peer stages the next key"); break;
                    case "rotate": peers[a]?.Rotate(); sim.Note($"host: peer{a} rotates"); break;
                    case "retire": foreach (var p in peers) p?.Retire(); sim.Note("host: every peer retires the previous key"); break;
                    case "stale_reboot": peers[a] = Peer.Restore(images[a]!, 3); sim.Note($"host: peer{a} reboots from its boot image at epoch 3, which its storage never kept"); break;
                    case "torn_save": {
                        var latest = peers[a]!.Save(); int half = images[a]!.Length / 2;
                        var torn = latest.Take(half).Concat(images[a]!.Skip(half)).ToArray();
                        try { peers[a] = Peer.Restore(torn, 3); sim.Note($"host: peer{a} reboots from a torn block, accepted"); }
                        catch (MudException ex) when (ex.Code == -4) { sim.Note($"host: peer{a} reboots from a torn block, refused as Corrupt; booting from the boot image"); peers[a] = Peer.Restore(images[a]!, 3); }
                        break;
                    }
                    case "hostile": sim.Note($"host: peer{a} calls a hostile method"); if (peers[a] is Peer h && Refused(() => h.Call(now, "nav", e.Text, Encoding.UTF8.GetBytes("x"), 20)) is string he) sim.Note($"peer{a} refused call {he}"); break;
                    case "revoke": if (Signed) peers[a]?.Revoke((uint)b + 1); sim.Note($"host: peer{a} revokes peer{b}'s roster line"); break;
                    case "telemetry":
                        sim.Note("host: every drone publishes its position to gcs");
                        for (int i = 1; i < n; i++) if (peers[i] is Peer d && Refused(() => d.Publish(now, "gcs", Encoding.ASCII.GetBytes("pos"), 600)) is string te) sim.Note($"peer{i} refused publish {te}");
                        break;
                    case "flood": flooding[a] = b > 0; break;
                    case "warp": sim.Note($"host: the clock jumps {e.A} ms"); sim.Step(e.A); t += e.A; end = Math.Max(end, t + 50_000); warped = true; break;
                }
                if (warped) break;
            }
            if (warped) continue;
            if (peers[0] is Peer gcs)
            {
                if (round == 0)
                {
                    gcs.Call(now, "nav", "Ping", Encoding.ASCII.GetBytes("1"), 60); gcs.Call(now, "nav", "Ping", Encoding.ASCII.GetBytes("2"), 10);
                    sim.Note("gcs calls Ping#1 ttl 60 and Ping#2 ttl 10");
                }
                else if (round % 150 == 0)
                {
                    ping++;
                    sim.Note(Refused(() => gcs.Call(now, "nav", "Ping", Encoding.ASCII.GetBytes(ping.ToString()), 20)) is string ge ? $"peer0 refused call {ge}" : $"gcs calls Ping#{ping} ttl 20");
                }
            }
            for (int i = 0; i < n; i++)
            {
                if (peers[i] is not Peer p) continue;
                var calls = new List<Incoming>();
                var notes = new List<(string Text, bool Kinded)>();
                if (spam.TryGetValue(i, out var state))
                    for (uint k = 0; k < state[0] * 100 / 1000; k++)
                    {
                        uint clock = ++state[1];
                        if (Refused(() => p.Publish(clock, "nav", Encoding.ASCII.GetBytes("spam"), 5)) is string se) notes.Add(($"peer{i} refused publish {se}", false));
                    }
                while (p.Outgoing(now) is Outgoing o)
                {
                    bool ok = o.Listen ? sim.Listen(i, o.Topic) : sim.Send(i, o.Topic, o.Wire, o.TtlSecs);
                    if (!ok) break;
                    p.Sent();
                }
                while (sim.Poll(i) is var rx && rx != null)
                {
                    if (rx.Value.Kind != 1) continue;
                    var inc = p.Deliver(now, rx.Value.Topic, rx.Value.Wire);
                    switch (inc.Kind)
                    {
                        case Kind.Call: notes.Add(($"peer{i} call {Q(inc.Method)} {Q(inc.Bytes)}", true)); calls.Add(inc); break;
                        case Kind.Reply: notes.Add(($"peer{i} reply seq={inc.Seq} end={(inc.End ? "true" : "false")} status={inc.Status} {Q(inc.Bytes)}", true)); break;
                        case Kind.Event: landed[i]++; notes.Add(($"peer{i} event {Q(inc.Topic)} {Q(inc.Bytes)} from sender {inc.Sender}", true)); break;
                        case Kind.Flood: notes.Add(($"peer{i} flood from sender {inc.Sender}", false)); break;
                        case Kind.Rollback: notes.Add(($"peer{i} rollback from sender {inc.Sender}", false)); break;
                        case Kind.Dropped: notes.Add(($"peer{i} dropped {DroppedName(inc)}", false)); break;
                    }
                }
                while (p.Tick(now) != null) notes.Add(($"peer{i} expired", false));
                string via = p.Opened switch { Slot.Previous => " via Previous", Slot.Next => " via Next", _ => "" };
                foreach (var c in calls) if (Refused(() => p.Reply(now, c, 0, c.Bytes)) is string re) notes.Add(($"peer{i} refused reply {re}", false));
                for (int k = 0; k < notes.Count;)
                {
                    int run = 1;
                    while (k + run < notes.Count && notes[k + run].Text == notes[k].Text) run++;
                    var (text, kinded) = notes[k];
                    sim.Note(run > 1 ? $"{text} ×{run}" : kinded ? text + via : text);
                    k += run;
                }
            }
            sim.Step(100);
            t += 100; round++;
        }
        for (int i = 0; i < n; i++) if (peers[i] is Peer p) sim.Note($"peer{i} pressure {p.Pressure}");
        var stdout = Console.OpenStandardOutput();
        var bytes = new UTF8Encoding(false).GetBytes(sim.Log());
        stdout.Write(bytes, 0, bytes.Length);
        return 0;
    }
}
