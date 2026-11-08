// The standard harness in Java: must produce the same log as adapters/harness.rs, byte for byte.
// usage: java --enable-native-access=ALL-UNNAMED Harness <scenario file> [--signed] [--gcm]
import mudrpc.Mud.*;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.*;

import static java.nio.charset.StandardCharsets.UTF_8;

public final class Harness {
    static final String[] OPEN = { "", "Short", "Tag", "Full", "Sig", "Roster", "Budget" };
    static final String[] DECODE = { "", "Codec(Truncated)", "Codec(Overflow)", "Codec(Tag)", "Codec(Full)", "Codec(Utf8)", "Missing", "Id" };
    static final byte[] KEY = counting(0), NEXT_KEY = counting(32);
    static byte[] counting(int start) { byte[] k = new byte[32]; for (int i = 0; i < 32; i++) k[i] = (byte) (start + i); return k; }   // as every harness derives them; a constant secret is refused

    static String droppedName(Incoming inc) {
        return switch (inc.dropped()) {
            case VERSION -> "Version(" + inc.status() + ")";
            case OPEN -> inc.status() >= 1 && inc.status() <= 6 ? "Open(" + OPEN[inc.status()] + ")" : "?";
            case DECODE -> "Decode(" + (inc.status() < 8 ? DECODE[inc.status()] : "?") + ")";
            case DUPLICATE -> "Duplicate"; case NO_PENDING -> "NoPending"; case TOPIC -> "Topic"; case SEQ -> "Seq";
            case ID -> "Id"; case REPLAY -> "Replay"; case RATE -> "Rate"; default -> "?";
        };
    }

    // Rust's {:?}: quotes and backslash escaped, tab, return, newline and nul by name, other ASCII controls as a braced hex escape (spelled out: a backslash-u in a Java comment is itself an escape)
    static String q(String s) {
        StringBuilder o = new StringBuilder("\"");
        for (char ch : s.toCharArray()) {
            switch (ch) {
                case '"' -> o.append("\\\""); case '\\' -> o.append("\\\\"); case '\n' -> o.append("\\n"); case '\r' -> o.append("\\r");
                case '\t' -> o.append("\\t"); case '\0' -> o.append("\\0");
                default -> { if (ch < 0x20 || ch == 0x7f) o.append("\\u{").append(Integer.toHexString(ch)).append('}'); else o.append(ch); }
            }
        }
        return o.append('"').toString();
    }
    static String q(byte[] b) { return q(new String(b, UTF_8)); }

    // rpc::Error's name if the peer refuses a call, publish or reply, as the Rust harness logs it; null if it took it
    static String refused(Runnable r) {
        try { r.run(); return null; }
        catch (MudException e) { return switch (e.code) { case -2 -> "Full"; case -3 -> "TooLarge"; case -4 -> "Corrupt"; case -6 -> "Epoch"; case -7 -> "Unprovisioned"; case -8 -> "Exhausted"; case -9 -> "Rate"; default -> "?"; }; }
    }

    static boolean SIGNED, GCM; static int PEERS;
    static Peer newPeer(String name, int sender, int epoch, boolean listenNav) {
        byte[] seed = null;
        if (SIGNED) seed = counting(sender);
        Peer p = new Peer(name, KEY, sender, epoch, seed, 1000, SIGNED && sender == 1, GCM ? 1 : 0);   // gcs is the hub
        if (SIGNED) for (int s = 1; s <= PEERS; s++) { try { p.roster(s, Peer.publicKey(counting(s)), null); } catch (MudException e) { if (e.isFull()) break; throw e; } }   // a leaf believes the hub and the drones that fit
        if (listenNav) p.listen("nav");
        return p;
    }

    record Note(String text, boolean kinded) {}

    public static void main(String[] args) throws Exception {
        Sim sim = new Sim(Files.readString(Path.of(args[0])));
        int n = sim.peers();
        SIGNED = List.of(args).contains("--signed"); GCM = List.of(args).contains("--gcm"); PEERS = n;
        String[] names = new String[n];
        for (int i = 0; i < n; i++) names[i] = i == 0 ? "gcs" : "drone-" + i;
        Set<String> hostKinds = Set.of("clone", "spam", "stage", "rotate", "retire", "warp", "hostile", "stale_reboot", "torn_save", "revoke", "telemetry", "flood");
        ArrayDeque<SimEvent> host = new ArrayDeque<>();
        sim.events().stream().filter(e -> hostKinds.contains(e.kind())).sorted(Comparator.comparingLong(SimEvent::atMs)).forEach(host::add);
        Set<Integer> clones = new HashSet<>();
        for (SimEvent e : host) if (e.kind().equals("clone")) clones.add((int) e.b());
        Peer[] peers = new Peer[n];
        for (int i = 0; i < n; i++) peers[i] = clones.contains(i) ? null : newPeer(names[i], i + 1, 2, i > 0);
        byte[][] images = new byte[n][];   // what each host wrote to flash at boot
        for (int i = 0; i < n; i++) images[i] = peers[i] == null ? null : peers[i].save();
        Map<Integer, int[]> spam = new LinkedHashMap<>();   // peer -> [rate, clock]
        boolean[] flooding = new boolean[n]; int[] landed = new int[n];   // radios flooding; events each peer took this second
        long end = sim.lastEventMs() + 50_000, t = 0;
        int ping = 2;
        long round = 0;   // gcs's cadence counts rounds, so a warp off the round grid keeps it
        while (t <= end) {
            int now = (int) (t / 1000);
            boolean warped = false;
            if (round % 10 == 0) {
                boolean any = false; for (boolean f : flooding) any |= f;
                for (int i = 0; i < n; i++) { if (landed[i] > 0 && any) sim.note("peer" + i + " honest events landed this second: " + landed[i]); landed[i] = 0; }
            }
            while (!host.isEmpty() && host.peek().atMs() <= t) {
                SimEvent e = host.poll();
                int a = (int) e.a(), b = (int) e.b();
                switch (e.kind()) {
                    case "clone" -> { peers[b] = newPeer(names[a], a + 1, 1, true); sim.note("host: radio " + b + " is a clone of peer" + a + " from an old image"); }
                    case "spam" -> { spam.remove(a); if (b > 0) spam.put(a, new int[] { b, 1_000_000 }); sim.note("host: peer" + a + " spams at " + b + "/s"); }
                    case "stage" -> { for (Peer p : peers) if (p != null) p.stage(NEXT_KEY); sim.note("host: every peer stages the next key"); }
                    case "rotate" -> { if (peers[a] != null) peers[a].rotate(); sim.note("host: peer" + a + " rotates"); }
                    case "stale_reboot" -> { peers[a] = Peer.restore(images[a], 3); sim.note("host: peer" + a + " reboots from its boot image at epoch 3, which its storage never kept"); }
                    case "torn_save" -> {
                        byte[] latest = peers[a].save(); int half = images[a].length / 2;
                        byte[] torn = new byte[images[a].length];
                        System.arraycopy(latest, 0, torn, 0, half); System.arraycopy(images[a], half, torn, half, images[a].length - half);
                        try { peers[a] = Peer.restore(torn, 3); sim.note("host: peer" + a + " reboots from a torn block, accepted"); }
                        catch (MudException ex) { sim.note("host: peer" + a + " reboots from a torn block, refused as Corrupt; booting from the boot image"); peers[a] = Peer.restore(images[a], 3); }
                    }
                    case "hostile" -> {
                        sim.note("host: peer" + a + " calls a hostile method");
                        Peer h = peers[a]; String why = h == null ? null : refused(() -> h.call(now, "nav", e.text(), "x".getBytes(UTF_8), 20));
                        if (why != null) sim.note("peer" + a + " refused call " + why);
                    }
                    case "retire" -> { for (Peer p : peers) if (p != null) p.retire(); sim.note("host: every peer retires the previous key"); }
                    case "revoke" -> { if (SIGNED && peers[a] != null) peers[a].revoke(b + 1); sim.note("host: peer" + a + " revokes peer" + b + "'s roster line"); }
                    case "telemetry" -> {
                        sim.note("host: every drone publishes its position to gcs");
                        for (int i = 1; i < n; i++) { Peer d = peers[i]; String why = d == null ? null : refused(() -> d.publish(now, "gcs", "pos".getBytes(UTF_8), 600)); if (why != null) sim.note("peer" + i + " refused publish " + why); }
                    }
                    case "flood" -> flooding[a] = b > 0;
                    case "warp" -> { sim.note("host: the clock jumps " + e.a() + " ms"); sim.step(e.a()); t += e.a(); end = Math.max(end, t + 50_000); warped = true; }
                }
                if (warped) break;
            }
            if (warped) continue;
            Peer gcs = peers[0];
            if (gcs != null) {
                if (round == 0) {
                    gcs.call(now, "nav", "Ping", "1".getBytes(UTF_8), 60); gcs.call(now, "nav", "Ping", "2".getBytes(UTF_8), 10);
                    sim.note("gcs calls Ping#1 ttl 60 and Ping#2 ttl 10");
                } else if (round % 150 == 0) {
                    ping++;
                    byte[] body = Integer.toString(ping).getBytes(UTF_8);
                    String why = refused(() -> gcs.call(now, "nav", "Ping", body, 20));
                    sim.note(why != null ? "peer0 refused call " + why : "gcs calls Ping#" + ping + " ttl 20");
                }
            }
            for (int i = 0; i < n; i++) {
                Peer p = peers[i];
                if (p == null) continue;
                List<Incoming> calls = new ArrayList<>();
                List<Note> notes = new ArrayList<>();
                int[] state = spam.get(i);
                if (state != null)
                    for (int k = 0; k < state[0] * 100 / 1000; k++) {
                        int clock = ++state[1];
                        String why = refused(() -> p.publish(clock, "nav", "spam".getBytes(UTF_8), 5));
                        if (why != null) notes.add(new Note("peer" + i + " refused publish " + why, false));
                    }
                Outgoing o;
                while ((o = p.outgoing(now)) != null) {
                    boolean ok = o.listen() ? sim.listen(i, o.topic()) : sim.send(i, o.topic(), o.wire(), o.ttlSecs());
                    if (!ok) break;
                    p.sent();
                }
                Sim.Rx rx;
                while ((rx = sim.poll(i)) != null) {
                    if (rx.kind() != 1) continue;
                    Incoming inc = p.deliver(now, rx.topic(), rx.wire());
                    switch (inc.kind()) {
                        case CALL -> { notes.add(new Note("peer" + i + " call " + q(inc.method()) + " " + q(inc.bytes()), true)); calls.add(inc); }
                        case REPLY -> notes.add(new Note("peer" + i + " reply seq=" + inc.seq() + " end=" + inc.end() + " status=" + inc.status() + " " + q(inc.bytes()), true));
                        case EVENT -> { landed[i]++; notes.add(new Note("peer" + i + " event " + q(inc.topic()) + " " + q(inc.bytes()) + " from sender " + inc.sender(), true)); }
                        case FLOOD -> notes.add(new Note("peer" + i + " flood from sender " + inc.sender(), false));
                        case ROLLBACK -> notes.add(new Note("peer" + i + " rollback from sender " + inc.sender(), false));
                        case DROPPED -> notes.add(new Note("peer" + i + " dropped " + droppedName(inc), false));
                    }
                }
                while (p.tick(now) != null) notes.add(new Note("peer" + i + " expired", false));
                String via = switch (p.opened()) { case PREVIOUS -> " via Previous"; case NEXT -> " via Next"; default -> ""; };
                for (Incoming c : calls) { String why = refused(() -> p.reply(now, c, 0, c.bytes())); if (why != null) notes.add(new Note("peer" + i + " refused reply " + why, false)); }
                for (int k = 0; k < notes.size();) {
                    int run = 1;
                    while (k + run < notes.size() && notes.get(k + run).text().equals(notes.get(k).text())) run++;
                    Note note = notes.get(k);
                    sim.note(run > 1 ? note.text() + " ×" + run : note.kinded() ? note.text() + via : note.text());
                    k += run;
                }
            }
            sim.step(100);
            t += 100; round++;
        }
        for (int i = 0; i < n; i++) if (peers[i] != null) sim.note("peer" + i + " pressure " + peers[i].pressure());
        System.out.writeBytes(sim.log().getBytes(UTF_8));
        System.out.flush();
    }
}
