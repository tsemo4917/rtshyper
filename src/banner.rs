static LOGO: &str = r#"
 ____ _____ ____  _
|  _ \_   _/ ___|| |__  _   _ _ __   ___ _ __ 
| |_) || | \___ \| '_ \| | | | '_ \ / _ \ '__|
|  _ < | |  ___) | | | | |_| | |_) |  __/ |   
|_| \_\|_| |____/|_| |_|\__, | .__/ \___|_|   
                        |___/|_|
"#;

pub fn init() {
    print!("{}", LOGO);
}