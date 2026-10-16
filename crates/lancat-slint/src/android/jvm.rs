//! Rust → Java（app.lancat.Bridge 的静态方法）。Java 不调 Rust：分享 / 通知按钮 / 返回键等事件由 Rust 轮询取走
//! （`poll` / `take_actions`），所以没有 JNI native 方法要注册，也没有 Java 线程进 Rust。
//!
//! * 类用 Activity 的 ClassLoader 加载一次，存成 GlobalRef：原生线程（android_main、tokio、通知线程）上
//!   FindClass 只能看到系统类。
//! * 任何线程都可以调：`attach_current_thread_permanently`（线程退出时 jni 自己 detach），每次调用包一层
//!   local frame，长驻线程不会攒 local ref。
//! * Java 抛异常：打印后清掉，返回 None / 默认值，不让 Rust 侧 panic。

use jni::objects::{GlobalRef, JClass, JObject, JString, JValue};
use jni::{JNIEnv, JavaVM};
use std::sync::OnceLock;

static VM: OnceLock<JavaVM> = OnceLock::new();
static CLS: OnceLock<GlobalRef> = OnceLock::new();

/// android_main 里调用（ndk_context 此时指向当前 Activity）。进程里只做一次。
pub fn init() -> Result<(), String> {
    if CLS.get().is_some() {
        return Ok(());
    }
    let ctx = ndk_context::android_context();
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| e.to_string())?;
    let g = {
        let mut jenv = vm.attach_current_thread_permanently().map_err(|e| e.to_string())?;
        // Activity 的 jobject 归 android-activity 所有（global ref），这里只借用，不删除
        let act = unsafe { JObject::from_raw(ctx.context().cast()) };
        let r: jni::errors::Result<GlobalRef> = jenv.with_local_frame(8, |e| {
            let loader = e.call_method(&act, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?.l()?;
            let name = e.new_string("app.lancat.Bridge")?;
            let cls = e.call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])?.l()?;
            e.new_global_ref(cls)
        });
        clear_exc(&mut jenv);
        r.map_err(|e| format!("load app.lancat.Bridge: {e}"))?
    };
    let _ = CLS.set(g);
    let _ = VM.set(vm);
    Ok(())
}

fn clear_exc(e: &mut JNIEnv) {
    if e.exception_check().unwrap_or(false) {
        let _ = e.exception_describe();
        let _ = e.exception_clear();
    }
}

fn with_jni<R>(what: &str, f: impl FnOnce(&mut JNIEnv, &JClass) -> jni::errors::Result<R>) -> Option<R> {
    let vm = VM.get()?;
    let cls = CLS.get()?;
    let mut jenv = vm.attach_current_thread_permanently().ok()?;
    let cls: &JClass = cls.as_obj().into();
    let r = jenv.with_local_frame(16, |e| f(e, cls));
    clear_exc(&mut jenv);
    match r {
        Ok(v) => Some(v),
        Err(err) => {
            log::warn!("jni {what}: {err}");
            None
        }
    }
}

fn get_str(e: &mut JNIEnv, o: JObject) -> jni::errors::Result<String> {
    if o.is_null() {
        return Ok(String::new());
    }
    let s = JString::from(o);
    let r: String = e.get_string(&s)?.into();
    Ok(r)
}

/// 无参、返回 String 的静态方法
fn call_s(name: &str) -> String {
    with_jni(name, |e, c| {
        let o = e.call_static_method(c, name, "()Ljava/lang/String;", &[])?.l()?;
        get_str(e, o)
    })
    .unwrap_or_default()
}

fn call_z(name: &str) -> bool {
    with_jni(name, |e, c| e.call_static_method(c, name, "()Z", &[])?.z()).unwrap_or(false)
}

fn call_v(name: &str) {
    with_jni(name, |e, c| e.call_static_method(c, name, "()V", &[])?.v());
}

/// 静态方法，参数全是 String（签名按个数生成），返回 void
fn call_strs(name: &str, args: &[&str]) {
    with_jni(name, |e, c| {
        let mut js: Vec<JObject> = Vec::with_capacity(args.len());
        for a in args {
            js.push(JObject::from(e.new_string(a)?));
        }
        let vals: Vec<JValue> = js.iter().map(JValue::Object).collect();
        let sig = format!("({})V", "Ljava/lang/String;".repeat(args.len()));
        e.call_static_method(c, name, sig, &vals)?.v()
    });
}

pub fn files_dir() -> String {
    call_s("filesDir")
}
pub fn receive_dir() -> String {
    call_s("receiveDir")
}
pub fn device_name() -> String {
    call_s("deviceName")
}
pub fn night_mode() -> bool {
    call_z("isNightMode")
}
pub fn is_foreground() -> bool {
    call_z("isForeground")
}
pub fn move_to_back() {
    call_v("moveToBack")
}
pub fn start_service() {
    call_v("startService")
}
pub fn play_chime() {
    call_v("playChime")
}
/// 浅色主题时状态栏 / 导航栏用深色图标
pub fn set_light_bars(light: bool) {
    with_jni("setLightBars", |e, c| e.call_static_method(c, "setLightBars", "(Z)V", &[JValue::Bool(light as u8)])?.v());
}
pub fn open_file(path: &str) {
    call_strs("openFile", &[path])
}
pub fn scan_file(path: &str) {
    call_strs("scanFile", &[path])
}
/// 待处理的界面事件（每行一个，字段用 \t 分隔，已转义）
pub fn poll() -> Vec<Vec<String>> {
    lines(&call_s("poll"))
}
/// 通知上的按钮（暂停 / 继续 / 取消 / 重试）
pub fn take_actions() -> Vec<Vec<String>> {
    lines(&call_s("takeActions"))
}
/// 诊断用的系统状态：key=value 每行一个
pub fn diag_info() -> Vec<(String, String)> {
    call_s("diagInfo").lines().filter_map(|l| l.split_once('=').map(|(a, b)| (a.to_string(), b.to_string()))).collect()
}
/// 诊断里的「修复」：notif / battery / service / wifi
pub fn fix(id: &str) {
    call_strs("fix", &[id])
}

/// 选择文件（mode 0 文件 1 文件夹 2 贴纸），阻塞到用户选完、内容已复制进缓存；返回本地路径
pub fn pick_blocking(mode: i32) -> Vec<String> {
    with_jni("pickBlocking", |e, c| {
        let o = e.call_static_method(c, "pickBlocking", "(I)Ljava/lang/String;", &[JValue::Int(mode)])?.l()?;
        get_str(e, o)
    })
    .map(|s| s.lines().map(unescape).filter(|l| !l.is_empty()).collect())
    .unwrap_or_default()
}

pub fn notify_message(key: &str, title: &str, text: &str) {
    call_strs("notifyMessage", &[key, title, text])
}

/// pct < 0 = 不确定进度；actions：逗号分隔（pause / resume / cancel）
pub fn notify_progress(id: &str, peer: &str, title: &str, text: &str, pct: i32, actions: &str) {
    with_jni("notifyProgress", |e, c| {
        let mut a: Vec<JObject> = Vec::new();
        for s in [id, peer, title, text, actions] {
            a.push(JObject::from(e.new_string(s)?));
        }
        e.call_static_method(
            c,
            "notifyProgress",
            "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;ILjava/lang/String;)V",
            &[JValue::Object(&a[0]), JValue::Object(&a[1]), JValue::Object(&a[2]), JValue::Object(&a[3]), JValue::Int(pct), JValue::Object(&a[4])],
        )?
        .v()
    });
}

/// 传完 / 失败：path 非空时点通知打开文件；actions 同上（retry）
pub fn notify_done(id: &str, peer: &str, title: &str, text: &str, path: &str, ok: bool, actions: &str) {
    with_jni("notifyDone", |e, c| {
        let mut a: Vec<JObject> = Vec::new();
        for s in [id, peer, title, text, path, actions] {
            a.push(JObject::from(e.new_string(s)?));
        }
        e.call_static_method(
            c,
            "notifyDone",
            "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;ZLjava/lang/String;)V",
            &[JValue::Object(&a[0]), JValue::Object(&a[1]), JValue::Object(&a[2]), JValue::Object(&a[3]), JValue::Object(&a[4]), JValue::Bool(ok as u8), JValue::Object(&a[5])],
        )?
        .v()
    });
}

pub fn cancel_notify(id: &str) {
    call_strs("cancelNotify", &[id])
}

fn lines(s: &str) -> Vec<Vec<String>> {
    s.lines().filter(|l| !l.is_empty()).map(|l| l.split('\t').map(unescape).collect()).collect()
}

/// Java 侧 Bridge.esc 的逆：\\ \n \t
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}
