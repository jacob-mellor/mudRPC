# JavaScript and TypeScript, and Connect over mudRPC

mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. The JavaScript binding is one file, `mud.js`, an ES module over the C ABI compiled to WebAssembly, with no package, no bundler and no TypeScript step: copy it into your project and your IDE sees every type and comment from the JSDoc. Node 22 or later, or a browser.

The document is [SKILL.md](SKILL.md): integrate step by step, verify, test at each rung, known edges.
