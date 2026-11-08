# AES-256-GCM

From FIPS 197 and SP 800-38D, bitsliced, no tables, behind the same seam as the signed sealer; the NIST and Wycheproof vectors are in `tests/`, and the gate counts the conditional branches where it touches secrets ([ci.sh](../ci.sh)).
