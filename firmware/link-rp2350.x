SECTIONS
{
    .start_block ORIGIN(FLASH) + 0x180 :
    {
        KEEP(*(.start_block));
    } > FLASH
}
