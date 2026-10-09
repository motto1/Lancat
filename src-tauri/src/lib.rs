#[tauri::command]
fn ping() -> &'static str {
    "pong"
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![ping])
        .setup(|app| {
            tauri::WebviewWindowBuilder::new(app, "panel", tauri::WebviewUrl::App("index.html".into()))
                .title("Lancat")
                .inner_size(360.0, 520.0)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Lancat");
}
