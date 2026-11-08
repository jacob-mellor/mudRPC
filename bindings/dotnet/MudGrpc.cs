// mudRPC as gRPC's transport on .NET: the client and the service base that Grpc.Tools generates from a .proto run over
// mudRPC unchanged; building the channel is the one line that differs. Beside Mud.cs, with the NuGet package
// Grpc.Core.Api, whose abstract types this implements (Copyright The gRPC Authors, Apache License 2.0).
//
// Not a drop-in where gRPC needs a connection: a message of about 460 bytes, no metadata, unary and server-streaming
// calls only, at most 63 items a stream, the first reply wins, and 16 calls in flight. A server sends 32 replies a
// second, half what a receiver takes from one sender, so a 63-item stream takes two; a client keeps under 32 calls a second. A deadline is the call's ttl, a week at most; a call cancelled after it
// was sent keeps its place among the 16 until its ttl, since nothing reaches the server to say so.
using System;
using System.Buffers;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using Grpc.Core;
#nullable enable

namespace MudRpc
{
    /// <summary>A gRPC channel over one peer: give it the peer and a send for its wires, feed it what arrives with <see cref="Deliver"/>; it pumps on its own thread.</summary>
    public sealed class MudChannel : ChannelBase, IDisposable
    {
        /// <summary>The detail of the reply that ends a server stream and carries no item. A reply with end, OK and no detail is the stream's last item, as the native API sends it.</summary>
        public const string StreamEnd = "end";
        const int Week = 604800, MaxItems = 63, Inbox = 1024, Tries = 20;   // a message's longest life, a stream's items, wires waiting for the pump, rounds a refused item is tried
        const int Pace = 32;   // replies a second: half what a receiver takes from one sender, so no edge of its second drops one
        static readonly Status Expired = new(StatusCode.DeadlineExceeded, "Deadline Exceeded"), Canceled = new(StatusCode.Cancelled, "Call canceled by the client.");
        static readonly System.Text.Encoding Strict = new System.Text.UTF8Encoding(false, true);   // a name with a lone surrogate is refused, not sent as U+FFFD
        readonly Peer peer;
        readonly Action<Outgoing> send;
        readonly Action<Exception> fault;
        readonly uint ttl;
        readonly object gate = new();   // one thread drives the peer at a time
        readonly Stopwatch clock = Stopwatch.StartNew();
        readonly ConcurrentQueue<(string Topic, byte[] Wire)> inbox = new();
        readonly Dictionary<string, Exchange> pending = new();
        readonly HashSet<Exchange> open = new();   // filed and not yet let go: pending, or a stream whose end has landed with items unread
        readonly Dictionary<(string, string), Func<Incoming, DateTime, Task>> methods = new();   // a call and the moment its ttl ends, counted from its arrival
        readonly HashSet<string> services = new();
        readonly Dictionary<uint, List<Owed>> answers = new();   // the replies owed, by sender
        readonly List<uint> order = new();   // the senders owed a reply, each once, the next to send first
        readonly AutoResetEvent wake = new(false);
        readonly Thread pump;
        volatile bool closed;
        volatile Udp? radio;
        uint paceSec, paced;
        int waiting, refusals, queued;   // queued: calls waiting for one of the 64 handlers
        readonly SemaphoreSlim handlers = new(64);

        /// <summary>The peer is the channel's while it lives: reach it only through <see cref="Use"/>, and dispose the channel before the peer. <paramref name="ttlSecs"/> is the life of a call made without a deadline; <paramref name="fault"/> hears every exception the transport throws, and the item it threw on is retried a second's worth of rounds, then let go.</summary>
        public MudChannel(Peer peer, Action<Outgoing> send, uint ttlSecs = 60, Action<Exception>? fault = null) : base("mudrpc")
        {
            this.peer = peer; this.send = send; this.fault = fault ?? (_ => { }); ttl = Math.Clamp(ttlSecs, 1, Week);
            Binder = new Bind(this);
            pump = new Thread(Pump) { IsBackground = true, Name = "mudrpc" };
            pump.Start();
        }

        /// <summary>Over a <see cref="Udp"/>, which the pump sends through and reads in its own round, so the product writes no loop; the socket stays the caller's to dispose, after the channel.</summary>
        public MudChannel(Peer peer, Udp udp, uint ttlSecs = 60, Action<Exception>? fault = null) : this(peer, udp.Send, ttlSecs, fault) { radio = udp; wake.Set(); }

        /// <summary>For the generated <c>BindService(ServiceBinderBase, ...Base)</c>: the service answers calls through this channel.</summary>
        public ServiceBinderBase Binder { get; }

        /// <summary>A wire from the transport, from any thread. Past 1024 waiting for the pump, a wire is dropped, as core would drop a flood.</summary>
        public void Deliver(string topic, byte[] wire)
        {
            ArgumentNullException.ThrowIfNull(topic); ArgumentNullException.ThrowIfNull(wire);
            if (closed || Interlocked.Increment(ref waiting) > Inbox) { Interlocked.Decrement(ref waiting); return; }
            inbox.Enqueue((topic, wire)); wake.Set();
        }

        /// <summary>The peer, between rounds of the pump: <c>Save</c>, <c>Stage</c>, <c>Rotate</c>, <c>Roster</c>, <c>Counts</c>.</summary>
        public T Use<T>(Func<Peer, T> act) { lock (gate) { var r = act(peer); wake.Set(); return r; } }

        /// <inheritdoc cref="Use{T}"/>
        public void Use(Action<Peer> act) => Use(p => { act(p); return 0; });

        public override CallInvoker CreateCallInvoker() => new Invoker(this);

        /// <summary>Stops the pump and fails every call in flight with <c>Unavailable</c>.</summary>
        public void Dispose()
        {
            lock (gate) Close(new Status(StatusCode.Unavailable, "the channel is closed"));
            wake.Set();
            if (Thread.CurrentThread != pump && !Monitor.IsEntered(gate)) pump.Join();   // from the transport's send or from Use, the pump ends after this round
        }

        protected override Task ShutdownAsyncCore() { Dispose(); return Task.CompletedTask; }

        uint Now => (uint)(clock.ElapsedMilliseconds / 1000);

        /// <summary>Under <see cref="gate"/>.</summary>
        void Close(Status s)
        {
            if (closed) return;
            closed = true;
            foreach (var x in new List<Exchange>(open)) x.Abort(s);
        }

        void Pump()
        {
            var calls = new List<(Func<Incoming, DateTime, Task>, Incoming, DateTime)>();
            while (true)
            {
                wake.WaitOne(radio == null ? 50 : 10);
                lock (gate)
                {
                    if (closed) return;
                    uint now = Now;
                    try
                    {
                        while (inbox.TryDequeue(out var w))
                        {
                            Interlocked.Decrement(ref waiting);
                            Incoming e;
                            try { e = peer.Deliver(now, w.Topic, w.Wire); } catch (MudException) { continue; }   // a wire the transport mangled: no verdict to act on
                            Land(e, calls);
                        }
                        if (radio is Udp u)
                        {
                            try { for (int i = 0; i < Inbox && u.Poll() is (string topic, byte[] wire); i++) { try { Land(peer.Deliver(now, topic, wire), calls); } catch (MudException) { } } }
                            catch (MudException e) { try { fault(e); } catch (Exception) { } }
                            catch (ObjectDisposedException) { Close(new Status(StatusCode.Unavailable, "the socket is closed")); return; }
                        }
                        for (Incoming? e; (e = peer.Tick(now)) != null;) Land(e, calls);
                        Answer(now);
                        for (Outgoing? o; (o = peer.Outgoing(now)) != null;)
                        {
                            try { send(o); }
                            catch (ObjectDisposedException) { Close(new Status(StatusCode.Unavailable, "the socket is closed")); return; }
                            catch (Exception e) { try { fault(e); } catch (Exception) { } if (++refusals < Tries) break; }   // the next round tries it again; past a second of rounds it is let go
                            refusals = 0;
                            peer.Sent();
                        }
                    }
                    catch (MudException e) when (e.Code is -1 or -4 or -5) { Close(new Status(StatusCode.Unavailable, "the peer is closed: " + e.Message)); return; }
                    catch (MudException) { }   // busy, held by a caller outside Use: the next round
                }
                foreach (var (run, call, until) in calls) _ = Task.Run(async () =>
                {
                    await handlers.WaitAsync().ConfigureAwait(false);
                    Interlocked.Decrement(ref queued);
                    try { if (DateTime.UtcNow < until) await run(call, until).ConfigureAwait(false); }   // its caller has given up: a server behind sheds the call, not the next one
                    finally { handlers.Release(); }
                });
                calls.Clear();
            }
        }

        void Land(Incoming e, List<(Func<Incoming, DateTime, Task>, Incoming, DateTime)> calls)
        {
            if (e.Kind is Kind.Reply or Kind.Expired && pending.TryGetValue(Convert.ToHexString(e.Id), out var x)) x.Land(e);
            else if (e.Kind == Kind.Call && (methods.TryGetValue((e.Topic, e.Method), out var run) || services.Contains(e.Topic)))   // a call on a service this channel does not serve is not its to answer
            {
                var until = DateTime.UtcNow.AddSeconds(e.TtlSecs);
                if (Interlocked.Increment(ref queued) <= Inbox) { calls.Add((run ?? NoMethod, e, until)); return; }
                Interlocked.Decrement(ref queued);
                using var ctx = new Context($"/{e.Topic}/{e.Method}", e, until);
                try { Send(ctx, (uint)StatusCode.ResourceExhausted, "the server is behind", Array.Empty<byte>(), true); } catch (RpcException) { }
            }
        }

        /// <summary>A call on a service this channel serves, for a method it does not: <c>Unimplemented</c>, paced as every reply.</summary>
        Task NoMethod(Incoming call, DateTime until)
        {
            using var ctx = new Context($"/{call.Topic}/{call.Method}", call, until);
            try { Send(ctx, (uint)StatusCode.Unimplemented, Clip($"no method {call.Topic}/{call.Method}"), Array.Empty<byte>(), true); }
            catch (RpcException) { }
            return Task.CompletedTask;
        }

        static string Clip(string? detail) => detail == null ? "" : detail.Length > 128 ? detail[..128] : detail;

        static Status Refused(MudException e) => new(e.Code switch { -2 or -3 or -9 => StatusCode.ResourceExhausted, -1 => StatusCode.InvalidArgument, _ => StatusCode.Internal }, e.Message);

        /// <summary>One call in flight: replies in any order, given to the caller in <c>seq</c> order, the end its status. Every method runs under <see cref="gate"/>, or before the call is filed.</summary>
        sealed class Exchange
        {
            readonly SortedDictionary<uint, Incoming> early = new();
            readonly List<Action> release = new();
            readonly Action<byte[]> item;
            readonly Action<Status> end;
            uint next;
            volatile bool done;
            public Action Forget = () => { };
            public readonly TaskCompletionSource<Metadata> Headers = new(TaskCreationOptions.RunContinuationsAsynchronously);
            public Status Status { get; private set; }
            public bool Done => done;
            bool released;
            public Action<Status>? Cut;   // a stream's: what a local end does to the items still unread
            public Action Unfile = () => { };
            public Exchange(Action<byte[]> item, Action<Status> end) { this.item = item; this.end = end; }

            /// <summary>A timer or a registration, let go at the end without waiting for its callback, which may be waiting for the gate this end holds.</summary>
            public void Hold(Action letGo) => release.Add(letGo);

            /// <summary>A local end, a cancel, a deadline or a close: it ends the call whatever the exchange, so a stream's unread items are cut off.</summary>
            public void Abort(Status s)
            {
                if (!done) { Finish(s); return; }
                if (released) return;
                Status = s;
                Cut?.Invoke(s);
                Release();
            }

            /// <summary>Lets the timers and registrations go, once: at the end, or for a stream when its reader has read or been cut off.</summary>
            public void Release()
            {
                if (released) return;
                released = true;
                Unfile();
                foreach (var letGo in release) letGo();
            }

            public void Land(Incoming e)
            {
                if (e.Kind == Kind.Expired) { Finish(Expired); return; }
                early.TryAdd(e.Seq, e);
                while (!done && early.Remove(next, out var r))
                {
                    next++;
                    if (r.Status != 0) Finish(new Status(r.Status <= 16 ? (StatusCode)r.Status : StatusCode.Unknown, r.Detail), true);
                    else if (r.End && r.Detail == StreamEnd) Finish(Status.DefaultSuccess, true);
                    else { Headers.TrySetResult(new Metadata()); item(r.Bytes); if (r.End) Finish(Status.DefaultSuccess, true); }
                }
            }

            public void Finish(Status s) => Finish(s, false);

            /// <summary><paramref name="sent"/>: the server's status, after the items before it; otherwise the channel's own.</summary>
            public void Finish(Status s, bool sent)
            {
                if (done) return;
                done = true; Status = s; Forget();
                if (Cut == null || !sent) Release();   // a stream the server ended keeps its timers until its reader has read
                if (s.StatusCode == StatusCode.OK) Headers.TrySetResult(new Metadata());
                else if (Headers.TrySetException(new RpcException(s))) _ = Headers.Task.Exception;   // observed here, so a caller who never awaits the headers raises no unobserved exception
                if (!sent && Cut != null) Cut(s); else end(s);   // a local end cuts a stream's unread items off
            }

            public Status Final() => done ? Status : throw new InvalidOperationException("Status can only be accessed once the call has finished.");
            public Metadata Trailers() => done ? new Metadata() : throw new InvalidOperationException("Trailers can only be accessed once the call has finished.");
        }

        /// <summary>Sends the request and files the exchange; a refusal, a passed deadline, a cancelled token or a closed channel finishes it at once, sending nothing.</summary>
        void Begin<TRequest, TResponse>(Exchange x, Method<TRequest, TResponse> method, CallOptions options, TRequest request) where TRequest : class where TResponse : class
        {
            double left = double.PositiveInfinity;
            if (options.Deadline is DateTime d && d != DateTime.MaxValue)
            {
                if (d.Kind != DateTimeKind.Utc && d != DateTime.MinValue) throw new InvalidOperationException("Deadline must have a kind DateTimeKind.Utc or be equal to DateTime.MaxValue or DateTime.MinValue.");
                left = (d - DateTime.UtcNow).TotalSeconds;
            }
            if (left <= 0) { x.Finish(Expired); return; }
            if (options.CancellationToken.IsCancellationRequested) { x.Finish(Canceled); return; }
            byte[] bytes;
            try { bytes = Encode(method.RequestMarshaller, request); }
            catch (Exception) { x.Finish(new Status(StatusCode.Internal, "Failed to serialize request message.")); return; }
            uint life = double.IsPositiveInfinity(left) ? ttl : (uint)Math.Min(Math.Ceiling(left) + 1, Week);   // a second over: the clock is whole seconds, the timer below is the deadline; past a week, a week
            lock (gate)
            {
                if (closed) { x.Finish(new Status(StatusCode.Unavailable, "the channel is closed")); return; }
                string key;
                try { if (Strict.GetByteCount(method.ServiceName) is 0 or > 64) throw new MudException(-1); Strict.GetByteCount(method.Name); key = Convert.ToHexString(peer.Call(Now, method.ServiceName, method.Name, bytes, life)); }
                catch (MudException e) { x.Finish(Refused(e)); return; }
                catch (ArgumentException) { x.Finish(Refused(new MudException(-1))); return; }
                pending[key] = x;
                open.Add(x);
                x.Forget = () => pending.Remove(key);
                x.Unfile = () => open.Remove(x);
                if (left <= Week)
                {
                    var timer = new Timer(_ => { lock (gate) x.Abort(Expired); }, null, TimeSpan.FromSeconds(left), Timeout.InfiniteTimeSpan);
                    x.Hold(() => timer.Dispose());
                }
                if (options.CancellationToken.CanBeCanceled)
                {
                    var reg = options.CancellationToken.Register(() => { lock (gate) x.Abort(Canceled); });
                    x.Hold(() => reg.Unregister());
                }
            }
            wake.Set();
        }

        /// <summary>A message through its marshaller's contextual entry points, which Google.Protobuf's generated code writes through.</summary>
        static byte[] Encode<T>(Marshaller<T> m, T message) { var c = new Into(); m.ContextualSerializer(message, c); return c.Bytes; }
        static T Decode<T>(Marshaller<T> m, byte[] bytes) => m.ContextualDeserializer(new From(bytes));

        sealed class Into : SerializationContext
        {
            readonly ArrayBufferWriter<byte> w = new();
            byte[]? whole;
            public byte[] Bytes => whole ?? w.WrittenSpan.ToArray();
            public override void Complete(byte[] payload) => whole = payload;
            public override IBufferWriter<byte> GetBufferWriter() => w;
            public override void SetPayloadLength(int payloadLength) { }
            public override void Complete() { }
        }

        sealed class From : DeserializationContext
        {
            readonly byte[] b;
            public From(byte[] b) => this.b = b;
            public override int PayloadLength => b.Length;
            public override byte[] PayloadAsNewBuffer() => (byte[])b.Clone();
            public override ReadOnlySequence<byte> PayloadAsReadOnlySequence() => new(b);
        }

        void Cancel(Exchange x) { lock (gate) x.Abort(Canceled); }
        void Drained(Exchange x) { lock (gate) x.Release(); }

        sealed class Invoker : CallInvoker
        {
            readonly MudChannel ch;
            public Invoker(MudChannel ch) => this.ch = ch;

            public override TResponse BlockingUnaryCall<TRequest, TResponse>(Method<TRequest, TResponse> method, string? host, CallOptions options, TRequest request) =>
                AsyncUnaryCall(method, host, options, request).ResponseAsync.GetAwaiter().GetResult();

            public override AsyncUnaryCall<TResponse> AsyncUnaryCall<TRequest, TResponse>(Method<TRequest, TResponse> method, string? host, CallOptions options, TRequest request)
            {
                var done = new TaskCompletionSource<TResponse>(TaskCreationOptions.RunContinuationsAsynchronously);
                TResponse? got = null;
                Exchange? x = null;
                x = new Exchange(b => { try { got ??= Decode(method.ResponseMarshaller, b); } catch (Exception) { x!.Finish(new Status(StatusCode.Internal, "Failed to deserialize response message.")); } },
                    s => { if (s.StatusCode != StatusCode.OK) done.TrySetException(new RpcException(s)); else if (got != null) done.TrySetResult(got); else done.TrySetException(new RpcException(new Status(StatusCode.Internal, "no response"))); });
                ch.Begin(x, method, options, request);
                return new AsyncUnaryCall<TResponse>(done.Task, x.Headers.Task, x.Final, x.Trailers, () => ch.Cancel(x));
            }

            public override AsyncServerStreamingCall<TResponse> AsyncServerStreamingCall<TRequest, TResponse>(Method<TRequest, TResponse> method, string? host, CallOptions options, TRequest request)
            {
                Exchange? x = null;
                var reader = new Reader<TResponse>(() => ch.Cancel(x!), () => ch.Drained(x!));
                x = new Exchange(b => { try { reader.Add(Decode(method.ResponseMarshaller, b)); } catch (Exception) { x!.Finish(new Status(StatusCode.Internal, "Failed to deserialize response message.")); } }, reader.End) { Cut = reader.Cut };
                ch.Begin(x, method, options, request);
                return new AsyncServerStreamingCall<TResponse>(reader, x.Headers.Task, x.Final, x.Trailers, () => ch.Cancel(x));
            }

            public override AsyncClientStreamingCall<TRequest, TResponse> AsyncClientStreamingCall<TRequest, TResponse>(Method<TRequest, TResponse> method, string? host, CallOptions options) =>
                throw new RpcException(new Status(StatusCode.Unimplemented, "mudRPC carries no client stream"));

            public override AsyncDuplexStreamingCall<TRequest, TResponse> AsyncDuplexStreamingCall<TRequest, TResponse>(Method<TRequest, TResponse> method, string? host, CallOptions options) =>
                throw new RpcException(new Status(StatusCode.Unimplemented, "mudRPC carries no client stream"));
        }

        /// <summary>A server stream's items in order; its end is the call's status, read after the items before it, and a local end cuts them off. A token cancelled while reading cancels the call.</summary>
        sealed class Reader<T> : IAsyncStreamReader<T>
        {
            readonly System.Threading.Channels.Channel<T> items = System.Threading.Channels.Channel.CreateUnbounded<T>();
            readonly Action cancel, drained;
            volatile RpcException? cut;
            public Reader(Action cancel, Action drained) { this.cancel = cancel; this.drained = drained; }
            public T Current { get; private set; } = default!;
            public void Add(T item) => items.Writer.TryWrite(item);
            public void End(Status s) => items.Writer.TryComplete(s.StatusCode == StatusCode.OK ? null : new RpcException(s));
            public void Cut(Status s) { cut = new RpcException(s); items.Writer.TryComplete(cut); }
            public async Task<bool> MoveNext(CancellationToken token)
            {
                if (cut is RpcException c) throw c;
                try { if (!await items.Reader.WaitToReadAsync(token).ConfigureAwait(false)) { drained(); return false; } }
                catch (OperationCanceledException) when (token.IsCancellationRequested) { cancel(); throw new RpcException(Canceled); }
                catch (RpcException) { drained(); throw; }
                if (cut is RpcException d) throw d;
                items.Reader.TryRead(out var v);
                Current = v!;
                return true;
            }
        }

        /// <summary>Runs one call's handler, then answers: the unary reply, the stream's end, or a status, an <c>RpcException</c>'s own, the context's, or gRPC's <c>Unknown</c> for any other exception.</summary>
        async Task Serve<TRequest, TResponse>(Method<TRequest, TResponse> method, Incoming call, DateTime until, Func<TRequest, Context, Task<byte[]?>> body) where TRequest : class where TResponse : class
        {
            using var ctx = new Context(method.FullName, call, until);
            (uint Status, string Detail, byte[] Payload) last;
            TRequest? req = null;
            try { req = Decode(method.RequestMarshaller, call.Bytes); } catch (Exception) { }
            if (req == null) last = ((uint)StatusCode.Internal, "Failed to deserialize request message.", Array.Empty<byte>());
            else
            {
                try
                {
                    var reply = await body(req, ctx).ConfigureAwait(false);
                    var set = ctx.Final;
                    last = set.StatusCode != StatusCode.OK ? ((uint)set.StatusCode, set.Detail, Array.Empty<byte>()) : reply != null ? (0u, "", reply) : (0u, StreamEnd, Array.Empty<byte>());
                }
                catch (RpcException e) { last = ((uint)e.StatusCode, e.Status.Detail, Array.Empty<byte>()); }
                catch (Exception) { last = ((uint)StatusCode.Unknown, "Exception was thrown by handler.", Array.Empty<byte>()); }
            }
            try { Send(ctx, last.Status, Clip(last.Detail), last.Payload, true); }
            catch (RpcException) { }   // closed, past its ttl, or its stream over: the caller has its status, or its call expires
        }

        sealed record Owed(Context Ctx, uint Seq, uint Status, string Detail, byte[] Payload, bool End);

        /// <summary>Owes one reply, which the pump sends in its caller's turn; a closed channel, a passed ttl or a stream over fails the write.</summary>
        void Send(Context ctx, uint status, string detail, byte[] payload, bool end)
        {
            lock (gate)
            {
                if (closed) throw new RpcException(new Status(StatusCode.Unavailable, "the channel is closed"));
                if (ctx.Over is Status over) throw new RpcException(over);
                if (DateTime.UtcNow >= ctx.Until) throw new RpcException(Expired);
                if (!end && ctx.Seq >= MaxItems) throw new RpcException(new Status(StatusCode.ResourceExhausted, $"a stream carries at most {MaxItems} items"));
                uint from = ctx.Call.Sender, seq = ctx.Seq++;   // a reply dropped spends its seq, so its caller waits out its ttl rather than reading past a gap
                if (!answers.TryGetValue(from, out var line)) answers[from] = line = new();
                if (line.Count >= Inbox) return;   // a sender owed sixteen full streams is flooding: its caller waits out its ttl
                if (answers.Values.Sum(l => l.Count) >= 4 * Inbox)   // 64 full streams owed: the longest line gives way, so one flooding sender cannot crowd out the rest
                {
                    var longest = answers.MaxBy(l => l.Value.Count);
                    if (longest.Key == from) return;
                    longest.Value.RemoveAt(longest.Value.Count - 1);
                }
                if (!order.Contains(from)) order.Add(from);
                line.Add(new Owed(ctx, seq, status, detail, payload, end));
            }
            wake.Set();
        }

        /// <summary>Under <see cref="gate"/>: 32 replies a second, a turn each, so a caller behind a crowd waits one turn, not the crowd; a full outbox leaves the rest for the next round, each until its call's ttl.</summary>
        void Answer(uint now)
        {
            if (now != paceSec) (paceSec, paced) = (now, 0);
            while (paced < Pace && order.Count > 0)
            {
                uint from = order[0];
                order.RemoveAt(0);
                var line = answers[from];
                if (line.Count == 0) { answers.Remove(from); continue; }
                var a = line[0];
                line.RemoveAt(0);
                try { if (DateTime.UtcNow < a.Ctx.Until) { peer.Reply(now, a.Ctx.Call, a.Status, a.Payload, a.End, a.Seq, a.Detail); paced++; } }
                catch (MudException e) when (e.Code is -2 or -9 or -10) { line.Insert(0, a); order.Insert(0, from); return; }
                catch (MudException e) when (e.Code == -3 && a.Payload.Length > 0)
                {
                    a.Ctx.Over = new Status(StatusCode.ResourceExhausted, "the reply is larger than a mudRPC wire");
                    line.RemoveAll(b => b.Ctx == a.Ctx);
                    line.Insert(0, a with { Status = (uint)StatusCode.ResourceExhausted, Detail = a.Ctx.Over.Value.Detail, Payload = Array.Empty<byte>(), End = true });
                }
                catch (MudException e) { try { fault(e); } catch (Exception) { } }
                if (line.Count == 0) answers.Remove(from); else order.Add(from);
            }
        }

        void Register(string service, string name, Func<Incoming, DateTime, Task> run)
        {
            lock (gate) { methods[(service, name)] = run; if (services.Add(service)) peer.Listen(service); }
            wake.Set();
        }

        sealed class Bind : ServiceBinderBase
        {
            readonly MudChannel ch;
            public Bind(MudChannel ch) => this.ch = ch;

            public override void AddMethod<TRequest, TResponse>(Method<TRequest, TResponse> method, UnaryServerMethod<TRequest, TResponse>? handler) =>
                ch.Register(method.ServiceName, method.Name, (call, until) => ch.Serve(method, call, until, async (req, ctx) => Encode(method.ResponseMarshaller, await handler!(req, ctx).ConfigureAwait(false))));

            public override void AddMethod<TRequest, TResponse>(Method<TRequest, TResponse> method, ServerStreamingServerMethod<TRequest, TResponse>? handler) =>
                ch.Register(method.ServiceName, method.Name, (call, until) => ch.Serve(method, call, until, async (req, ctx) =>
                {
                    await handler!(req, new Writer<TResponse>(ch, ctx, method.ResponseMarshaller), ctx).ConfigureAwait(false);
                    return null;
                }));

            public override void AddMethod<TRequest, TResponse>(Method<TRequest, TResponse> method, ClientStreamingServerMethod<TRequest, TResponse>? handler) => Refuse(method);
            public override void AddMethod<TRequest, TResponse>(Method<TRequest, TResponse> method, DuplexStreamingServerMethod<TRequest, TResponse>? handler) => Refuse(method);

            void Refuse<TRequest, TResponse>(Method<TRequest, TResponse> method) where TRequest : class where TResponse : class =>
                ch.Register(method.ServiceName, method.Name, (call, until) => ch.Serve<TRequest, TResponse>(method, call, until, (_, _) => throw new RpcException(new Status(StatusCode.Unimplemented, "mudRPC carries no client stream"))));
        }

        sealed class Writer<T> : IServerStreamWriter<T>
        {
            readonly MudChannel ch; readonly Context ctx; readonly Marshaller<T> m;
            public Writer(MudChannel ch, Context ctx, Marshaller<T> m) { this.ch = ch; this.ctx = ctx; this.m = m; }
            public WriteOptions? WriteOptions { get; set; }
            public Task WriteAsync(T message) { ch.Send(ctx, 0, "", Encode(m, message), false); return Task.CompletedTask; }
        }

        /// <summary>What a handler can know of its call: the method, the caller's roster number as its peer, and the deadline its ttl gives.</summary>
        sealed class Context : ServerCallContext, IDisposable
        {
            readonly string method;
            readonly CancellationTokenSource gone;
            public readonly Incoming Call;
            public uint Seq;
            public Status? Over;   // a stream cut short: its handler's writes fail
            public readonly DateTime Until;
            public Context(string method, Incoming call, DateTime until)
            {
                this.method = method; Call = call;
                Until = until;
                gone = new CancellationTokenSource(until - DateTime.UtcNow > TimeSpan.Zero ? until - DateTime.UtcNow : TimeSpan.Zero);
            }
            public Status Final => StatusCore;
            public void Dispose() => gone.Dispose();
            protected override string MethodCore => method;
            protected override string HostCore => "mudrpc";
            protected override string PeerCore => $"mudrpc:{Call.Sender}";
            protected override DateTime DeadlineCore => Until;
            protected override Metadata RequestHeadersCore { get; } = new();
            protected override CancellationToken CancellationTokenCore => gone.Token;
            protected override Metadata ResponseTrailersCore { get; } = new();
            protected override Status StatusCore { get; set; }
            protected override WriteOptions? WriteOptionsCore { get; set; }
            protected override AuthContext AuthContextCore { get; } = new(null, new Dictionary<string, List<AuthProperty>>());
            protected override ContextPropagationToken CreatePropagationTokenCore(ContextPropagationOptions? options) => throw new NotSupportedException("mudRPC propagates no context");
            protected override Task WriteResponseHeadersAsyncCore(Metadata responseHeaders) => Task.CompletedTask;
        }
    }
}
