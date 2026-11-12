fn main() {
    pyo3_build_config::use_pyo3_cfgs();
    let python = pyo3_build_config::get();
    let executable = python
        .executable()
        .expect("set PYO3_PYTHON to the benchmark interpreter");
    println!("cargo:rustc-env=SIGNAL_BENCH_PYTHON={executable}");
    if let Some(dir) = python.lib_dir() {
        println!("cargo:rustc-link-arg-benches=-L{dir}");
        println!("cargo:rustc-link-arg-benches=-Wl,-rpath,{dir}");
    }
    if let Some(name) = python.lib_name() {
        println!("cargo:rustc-link-arg-benches=-l{name}");
    }
}
