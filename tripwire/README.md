# The no-panic tripwire

Core, the sealers and the ABI without std, linked into a bare binary whose panic handler calls a C `abort` nothing defines: if any panic site survives optimisation, the link fails. The gate builds it for four embedded targets, measures its text and counts the branches where the GCM sealer touches secrets ([ci.sh](../ci.sh)).
