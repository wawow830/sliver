fn main() {
    if std::env::var_os("CARGO_CFG_TARGET_OS").as_deref() == Some(std::ffi::OsStr::new("linux")) {
        // Lua C modules leave lua_* references unresolved and bind them to the
        // vendored runtime in this executable when dlopen loads the module.
        println!("cargo:rustc-link-arg=-rdynamic");
        println!("cargo:rustc-link-lib=systemd");
    }
}
