ENTRY(_start)
SECTIONS
{
    . = TEXT_START;

    _image_start = ABSOLUTE(.);

    .text : {
        /* 入口必须固定在镜像起始处：QEMU -kernel 与 mkimage -e 都跳到 TEXT_START，
           不能依赖编译器的 section 发射顺序 */
        *(.text.boot.entry)
        *(.text.boot)
        *(.text*)
    }

    .rodata : {
        *(.rodata*)
    }


    .data : {
        *(.data*)
    }

    .bss (NOLOAD) : ALIGN(4096) {
        *(.bss.stack)

        . = ALIGN(4096);
        _bss_begin = .;
        *(.bss*)
        . = ALIGN(4096);
        _bss_end = .;
    }

    _image_end = ABSOLUTE(.);
    
    . = 0x400000000;
    CPU_BASE = ABSOLUTE(.);
    .cpu_private (NOLOAD) : ALIGN(4096) {
        *(.cpu_private)
    }
}
