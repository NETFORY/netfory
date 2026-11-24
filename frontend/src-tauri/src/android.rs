//! Мост Rust → Kotlin (только Android): полноэкранный входящий, Telecom-состояния, маршрут звука.
//! Вызывает статические методы `<package>.CallService.*(Context, String…)` через JNI;
//! класс грузится через ClassLoader контекста (FindClass из не-главного потока не видит классы
//! приложения). На остальных платформах — no-op.

#[cfg(target_os = "android")]
fn call_service(method: &str, args: &[&str]) -> Result<(), String> {
    use jni::objects::{JClass, JObject, JString, JValue};
    let ctx = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.map_err(|e| e.to_string())?;
    let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
    // контекст принадлежит ndk_context (global ref) — оборачиваем без владения
    let context = unsafe { JObject::from_raw(ctx.context().cast()) };
    let pkg = env.call_method(&context, "getPackageName", "()Ljava/lang/String;", &[]).and_then(|v| v.l()).map_err(|e| e.to_string())?;
    let pkg: String = env.get_string(&JString::from(pkg)).map_err(|e| e.to_string())?.into();
    let loader = env.call_method(&context, "getClassLoader", "()Ljava/lang/ClassLoader;", &[]).and_then(|v| v.l()).map_err(|e| e.to_string())?;
    let cls_name = env.new_string(format!("{pkg}.CallService")).map_err(|e| e.to_string())?;
    let cls = env
        .call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&cls_name)])
        .and_then(|v| v.l())
        .map_err(|e| e.to_string())?;
    let cls = JClass::from(cls);
    // сигнатура: (Context, String × N) → void
    let sig = format!("(Landroid/content/Context;{})V", "Ljava/lang/String;".repeat(args.len()));
    let mut jstrs = Vec::with_capacity(args.len());
    for a in args {
        jstrs.push(env.new_string(a).map_err(|e| e.to_string())?);
    }
    let mut jargs: Vec<JValue> = vec![JValue::Object(&context)];
    jargs.extend(jstrs.iter().map(|s| JValue::Object(s)));
    env.call_static_method(&cls, method, &sig, &jargs).map_err(|e| e.to_string())?;
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
        return Err("исключение в CallService".into());
    }
    Ok(())
}

/// Входящий звонок: полноэкранное уведомление с «Ответить/Отклонить» (Android), иначе no-op.
pub(crate) fn incoming_call(name: &str, room_id: &str) {
    #[cfg(target_os = "android")]
    if let Err(e) = call_service("showIncoming", &[name, room_id]) {
        crate::syslog::log("chat", format!("android: showIncoming: {e}"));
    }
    #[cfg(not(target_os = "android"))]
    let _ = (name, room_id);
}

/// Звонок принят/отклонён/пропущен: убрать уведомление и экран входящего (Android), иначе no-op.
pub(crate) fn call_ended() {
    #[cfg(target_os = "android")]
    if let Err(e) = call_service("hideIncoming", &[]) {
        crate::syslog::log("chat", format!("android: hideIncoming: {e}"));
    }
}

/// Состояние звонка → Telecom (self-managed ConnectionService): calling | active | ended(+reason).
pub(crate) fn call_state(room_id: &str, state: &str, reason: &str, peer_label: &str) {
    #[cfg(target_os = "android")]
    if let Err(e) = call_service("callState", &[room_id, state, reason, peer_label]) {
        crate::syslog::log("chat", format!("android: callState: {e}"));
    }
    #[cfg(not(target_os = "android"))]
    let _ = (room_id, state, reason, peer_label);
}

/// Маршрут звука текущего звонка: speaker | earpiece | bluetooth | wired.
pub(crate) fn audio_route(route: &str) -> Result<(), String> {
    #[cfg(target_os = "android")]
    return call_service("audioRoute", &[route]);
    #[cfg(not(target_os = "android"))]
    {
        let _ = route;
        Err("маршрут звука управляется системой".into())
    }
}
