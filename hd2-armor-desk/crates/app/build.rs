fn main() {
    println!("cargo:rerun-if-changed=assets/app-icon.ico");

    #[cfg(windows)]
    winres::WindowsResource::new()
        .set_icon("assets/app-icon.ico")
        .compile()
        .expect("embed application icon into Windows executable");
}
