# Adapter conformance

What a transport adapter must pass to be listed. We never write the real adapters; their communities do. The contract is
`adapters/transport.rs`: four calls, `send`, `listen`, `poll`, `silent`. The tests are the ones `sim` passes on both of
its profiles, in `adapters/tests/sim.rs` under "conformance". Copy that section, replace `world()` with your adapter's
constructor, keep every assertion.

1. `send` returns before anything is delivered, every time, including off-mesh.
2. `poll` on an idle peer returns `None` and keeps doing so without blocking.
3. Sending to a topic nobody listens on is not an error.
4. A message sent while partitioned arrives after heal if within ttl, and never arrives if ttl passed first, a jump of the clock included.
5. Two listeners on one topic both receive one publish. *Provisional for Reticulum:* its announced destinations are
   identity-bound; this is the first question for its maintainers, and the test may become "one listener per name".
6. `silent(true)`: nothing leaves the radio; messages still arrive.
7. Malformed bytes from a hostile radio reach `poll` as bytes; the transport does not interpret them.
8. A radio that is down cannot send (an error) and hears nothing until it is back.

Then the end-to-end check: `harness::run_with` over your transports on `scenarios/partition-heal.scenario` shows the same
peer events as the simulator's checked-in log (the lines containing `peer` and `gcs`), allowing the skew your rung has.
Over `lan` that is one step of clock skew; a second under a reticulum scenario, since plain UDP has no announce second;
and a heal under a partition another radio bridges, since `lan` has no relay hop and the wire waits for the heal. `lan`'s
clock is `as_millis() × speed`, quantised to `speed` milliseconds, so at SPEED 200 the ttl edge spans two rounds.
`adapters/tests/lan.rs` is the worked example of exactly this for a real-socket transport.

What the contract does not ask: encryption (every wire is sealed before it reaches you), ordering, deduplication,
acknowledgements, or any callback into mudRPC. If your transport needs more than the four calls, the design is wrong on
our side; open an issue rather than widening your adapter.
