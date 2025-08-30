use std::env::var;
use std::fs;
use std::io::{Result, Write};
use std::path::{Path, PathBuf};

struct ConfigPlatform {
    platform: &'static str,
    vm0_image_path: PathBuf,
    max_core_num: usize,
}

impl ConfigPlatform {
    fn gen_config_rs(&self) -> Result<Vec<u8>> {
        let mut output = vec![];
        writeln!(
            output,
            "pub const {variable}: {var_type} = {value};",
            variable = stringify!(CORE_NUM),
            var_type = stringify!(usize),
            value = self.max_core_num
        )?;
        Ok(output)
    }
}

fn get_config() -> ConfigPlatform {
    if cfg!(feature = "tx2") {
        ConfigPlatform {
            platform: "tx2",
            vm0_image_path: PathBuf::from("image/L4T"),
            max_core_num: 4,
        }
    } else if cfg!(feature = "pi4") {
        ConfigPlatform {
            platform: "pi4",
            vm0_image_path: PathBuf::from("image/Image_pi4_5.4.83_tlb"),
            max_core_num: 4,
        }
    } else if cfg!(feature = "qemu") {
        ConfigPlatform {
            platform: "qemu",
            vm0_image_path: PathBuf::from("image/Image_vanilla"),
            max_core_num: 8,
        }
    } else {
        panic!("Unsupported platform!");
    }
}

fn main() -> Result<()> {
    // set the linker script
    let arch = var("CARGO_CFG_TARGET_ARCH").unwrap();
    println!("cargo:rustc-link-arg=-Tlinkers/{arch}.ld");
    let config = get_config();
    println!("cargo:rustc-link-arg=--defsym=TEXT_START={}", env!("TEXT_START"));
    // set config file
    let out_dir = var("OUT_DIR").unwrap();
    let out_path = Path::new(&out_dir).join("config.rs");
    println!("Generating config file: {}", out_path.display());
    let config_rs = config.gen_config_rs()?;
    fs::write(out_path, config_rs)?;

    // set envs
    let build_time = chrono::offset::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
    println!("cargo:rustc-env=BUILD_TIME={}", build_time);
    let hostname = gethostname::gethostname();
    println!("cargo:rustc-env=HOSTNAME={}", hostname.into_string().unwrap());
    built::write_built_file().expect("Failed to acquire build-time information");
    let vm0_image_path = if config.vm0_image_path.is_relative() {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(&config.vm0_image_path)
    } else {
        config.vm0_image_path
    };
    println!("cargo:rustc-env=VM0_IMAGE_PATH={}", vm0_image_path.display());
    println!("cargo:rustc-env=PLATFORM={}", config.platform.to_uppercase());
    Ok(())
}
