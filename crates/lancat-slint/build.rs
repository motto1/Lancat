// slint UI + 内置 Noto 动态表情（src/assets/emoji/*.json，deflate 压缩后嵌入，运行时按需解压）
use std::io::Write;

fn main() {
    slint_build::compile("ui/app.slint").expect("compile slint ui");

    // Windows：把 assets/icon.ico 编进 exe 资源（ID 1），资源管理器和快捷方式显示应用图标
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        #[cfg(windows)]
        {
            let mut res = winresource::WindowsResource::new();
            res.set_icon("assets/icon.ico");
            res.compile().expect("embed windows icon");
        }
    }

    let dir = std::env::var("LANCAT_EMOJI_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../src/assets/emoji"));
    println!("cargo:rerun-if-env-changed=LANCAT_EMOJI_DIR");
    println!("cargo:rerun-if-changed={}", dir.display());
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let mut list: Vec<(String, std::path::PathBuf)> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let p = e.path();
                    (p.extension()? == "json").then(|| (p.file_stem().unwrap().to_string_lossy().to_string(), p))
                })
                .collect()
        })
        .unwrap_or_default();
    list.sort();
    let mut rs = String::from("pub static EMOJI_Z: &[(&str, &[u8])] = &[\n");
    for (code, p) in &list {
        let raw = std::fs::read(p).unwrap();
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(&raw).unwrap();
        let z = enc.finish().unwrap();
        let f = out.join(format!("{code}.z"));
        std::fs::write(&f, z).unwrap();
        rs += &format!("    ({code:?}, include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{code}.z\"))),\n");
    }
    rs += "];\n";
    std::fs::write(out.join("emoji_blobs.rs"), rs).unwrap();
}
