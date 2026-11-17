fn main() {
    // 图标属于 Windows 资源（EXE 的文件图标），但 tauri-build 只把 tauri.conf.json
    // 登记为依赖。只换图标、不动配置时构建脚本不会重跑，exe 里会留着旧图标，
    // 这里显式把图标文件登记进去。
    for name in [
        "icons/source.png",
        "icons/icon.ico",
        "icons/32x32.png",
        "icons/64x64.png",
        "icons/128x128.png",
        "icons/128x128@2x.png",
    ] {
        println!("cargo:rerun-if-changed={name}");
    }
    tauri_build::build()
}
