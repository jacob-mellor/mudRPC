# PHP, and gRPC PHP's client over mudRPC

mudRPC is RPC for networks that go away: a call is a message, a reply may come hours later or expire, nothing blocks. The PHP binding is one file, `Mud.php`, over the C ABI through PHP's FFI extension (`php -m` lists `FFI`; the default `ffi.enable=preload` lets the CLI load it, while php-fpm and Apache need `ffi.enable=1` or `Mud.php` in `opcache.preload`), with no composer package.

The document is [SKILL.md](SKILL.md): integrate step by step, verify, test at each rung, known edges.
