# The one protobuf mudRPC ships

`mud_options.proto`: two method options, `ttl_secs`, the lifetime of a call made through the method, and `topic`, a publish with no reply. Every other protobuf tool ignores them; `mud gen` reads them.
