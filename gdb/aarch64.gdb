target remote 127.0.0.1:1234
file target/aarch64-unknown-none-softfloat/debug/rtshyper_rs
break *0x40080000
display/i $pc
set confirm off
display/i $pc
set print asm-demangle on
