use crate::config::DEF_VM_CONFIG_TABLE;

pub fn vmm_init_config() {
    crate::config::config_init();
    println!("Apply default vm config");
    // let config_table = DEF_VM_CONFIG_TABLE.lock();
    // println!("entries num {}", config_table.entries.len());
    // if let Some(x) = config_table.entries[0].name {
    //     println!("{}", x);
    // }
    // println!("vm num {}", config_table.vm_num);

    println!("VM config init ok");
}