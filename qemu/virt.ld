/* QEMU's virt, a RISC-V board run with -bios none: 128 MB of RAM at 0x8000_0000, where QEMU loads the image and jumps; no
   flash, so the first 64 MB stand in for it. */
MEMORY { FLASH : ORIGIN = 0x80000000, LENGTH = 64M RAM : ORIGIN = 0x84000000, LENGTH = 64M }
INCLUDE sections.ld
