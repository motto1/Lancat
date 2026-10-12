//! 原生文件/文件夹选择（IFileOpenDialog，多选）。在独立 STA 线程里跑模态对话框，
//! 结果回到 UI 线程（避免对话框的嵌套消息循环重入 Slint 事件处理）。

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::System::Com::*;
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::*;

fn run(directory: bool, title: &str, filters: &[(String, String)]) -> Vec<String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let mut out = Vec::new();
        let r: windows::core::Result<()> = (|| {
            let dlg: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
            let mut opts = dlg.GetOptions()? | FOS_ALLOWMULTISELECT | FOS_FORCEFILESYSTEM;
            if directory {
                opts |= FOS_PICKFOLDERS;
            }
            dlg.SetOptions(opts)?;
            dlg.SetTitle(&HSTRING::from(title))?;
            let wide: Vec<(HSTRING, HSTRING)> = filters.iter().map(|(n, p)| (HSTRING::from(n.as_str()), HSTRING::from(p.as_str()))).collect();
            let specs: Vec<COMDLG_FILTERSPEC> = wide.iter().map(|(n, p)| COMDLG_FILTERSPEC { pszName: PCWSTR(n.as_ptr()), pszSpec: PCWSTR(p.as_ptr()) }).collect();
            if !specs.is_empty() {
                dlg.SetFileTypes(&specs)?;
            }
            dlg.Show(None)?;
            let items = dlg.GetResults()?;
            for i in 0..items.GetCount()? {
                let it = items.GetItemAt(i)?;
                let p = it.GetDisplayName(SIGDN_FILESYSPATH)?;
                out.push(p.to_string().unwrap_or_default());
                CoTaskMemFree(Some(p.0 as *const _));
            }
            Ok(())
        })();
        if let Err(e) = r {
            // 用户取消 = ERROR_CANCELLED (0x800704C7)
            if e.code().0 as u32 != 0x800704C7 {
                log::warn!("file dialog: {e}");
            }
        }
        CoUninitialize();
        out
    }
}

fn spawn(directory: bool, title: &'static str, filters: Vec<(String, String)>, done: impl FnOnce(Vec<String>) + Send + 'static) {
    std::thread::Builder::new()
        .name("lancat-filedlg".into())
        .spawn(move || {
            let paths = run(directory, title, &filters);
            let _ = slint::invoke_from_event_loop(move || done(paths));
        })
        .ok();
}

/// 选择要发送的文件 / 文件夹（多选）
pub fn pick(directory: bool, done: impl FnOnce(Vec<String>) + Send + 'static) {
    spawn(directory, if directory { "选择要发送的文件夹" } else { "选择要发送的文件" }, vec![], done);
}

/// 选择文件（带类型过滤），如导入贴纸
pub fn pick_filtered(filters: &[(&str, &str)], done: impl FnOnce(Vec<String>) + Send + 'static) {
    let f = filters.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    spawn(false, "导入贴纸", f, done);
}

/// 选择单个文件夹（接收目录）
pub fn pick_folder(done: impl FnOnce(Option<String>) + Send + 'static) {
    std::thread::Builder::new()
        .name("lancat-filedlg".into())
        .spawn(move || {
            let paths = run(true, "选择接收文件夹", &[]);
            let _ = slint::invoke_from_event_loop(move || done(paths.into_iter().next()));
        })
        .ok();
}
