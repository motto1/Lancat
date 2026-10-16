// 与 crates/lancat-slint/build.rs 相同（同一份 .slint 和内置动态表情），只是路径指向 lancat-slint。
// Windows 图标资源与 Android 无关，不编。
use std::io::Write;

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let ui = root.join("../lancat-slint/ui/app.slint");
    println!("cargo:rerun-if-changed={}", root.join("../lancat-slint/ui").display());
    slint_build::compile(&ui).expect("compile slint ui");

    let dir = std::env::var("LANCAT_EMOJI_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| root.join("../../src/assets/emoji"));
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
