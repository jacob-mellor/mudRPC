/* The image every board shares: the head and the vector table first, the stack from the end of RAM down. */
ENTRY(reset)
_stack_top = ORIGIN(RAM) + LENGTH(RAM);
SECTIONS {
  .text ORIGIN(FLASH) : {
    KEEP(*(.head));
    KEEP(*(.vectors));
    *(.text .text.*);
    *(.rodata .rodata.*);
  } > FLASH
  .ARM.exidx : { *(.ARM.exidx .ARM.exidx.*) } > FLASH
  .data : ALIGN(4) { _sdata = .; *(.data .data.*); . = ALIGN(4); _edata = .; } > RAM AT > FLASH
  _sidata = LOADADDR(.data);
  .bss (NOLOAD) : ALIGN(4) { _sbss = .; *(.bss .bss.*); *(COMMON); . = ALIGN(4); _ebss = .; } > RAM
}
/* The scrub checks clear 48 KiB of stack below their frame and search 32 KiB: the stack has 64 KiB above .bss at least. */
ASSERT(_stack_top - _ebss >= 64K, "the stack is under the 64 KiB the scrub checks clear")
