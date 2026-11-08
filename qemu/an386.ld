/* QEMU's mps2-an386, a Cortex-M4: 4 MB of SSRAM at 0 for code, 4 MB at 0x2000_0000 for data. */
MEMORY { FLASH : ORIGIN = 0x00000000, LENGTH = 4M RAM : ORIGIN = 0x20000000, LENGTH = 4M }
INCLUDE sections.ld
