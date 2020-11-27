use bindgen::Builder;
use std::{
    env,
    path::PathBuf,
};

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");

    let bindings = Builder::default()
        .clang_arg("-I../../../submodules/dpdk/build/include")
        .header("wrapper.h")
        .generate_inline_functions(true)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks))
        .blacklist_type("rte_arp_ipv4")
        .blacklist_type("rte_arp_hdr")
        .generate()
        .expect("Failed to generate bindings");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Failed to write bindings");
}
