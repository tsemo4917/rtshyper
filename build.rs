use std::env::var;

fn main() {
    // compile libfdt-bingding
    let c_compiler = "aarch64-none-elf-gcc";
    let fdt_dirs = ["libfdt-binding", "deps/libfdt"];
    let c_files = fdt_dirs.iter().flat_map(|path| {
        std::fs::read_dir(path).unwrap().filter_map(|f| {
            let f = f.unwrap();
            if f.file_type().unwrap().is_file() && matches!(f.path().extension(), Some(ext) if ext == "c") {
                Some(f.path())
            } else {
                None
            }
        })
    });
    cc::Build::new()
        .compiler(c_compiler)
        .includes(fdt_dirs)
        .files(c_files)
        .flag("-w")
        .compile("fdt-binding");

    // set the linker script
    let arch = var("CARGO_CFG_TARGET_ARCH").unwrap();
    let (platform, text_start) = if cfg!(feature = "tx2") {
        ("tx2", 0x83000000_u64)
    } else if cfg!(feature = "pi4") {
        ("pi4", 0xf0080000_u64)
    } else if cfg!(feature = "qemu") {
        ("qemu", 0x40080000_u64)
    } else {
        panic!("Unsupported platform!");
    };
    println!("cargo:rustc-link-arg=-Tlinkers/{arch}.ld");
    println!("cargo:rustc-link-arg=--defsym=TEXT_START={text_start}");

    println!("cargo:rerun-if-changed=src/");

    // set envs
    let build_time = chrono::offset::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
    println!("cargo:rustc-env=BUILD_TIME={}", build_time);
    println!("cargo:rustc-env=PLATFORM={}", platform.to_uppercase());
}
