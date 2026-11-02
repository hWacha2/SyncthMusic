//! Уведомление Android о новых файлах.
//!
//! Синхронизация пишет файлы обычными вызовами `std::fs`, поэтому медиатека
//! Android узнаёт о них только после сканирования. На ПК этого шага нет, а на
//! телефоне без него трек не появится в плеере, даже если файл уже лежит на
//! диске.
//!
//! Сканируем каталог библиотеки: `MediaScannerConnection.scanFile` принимает и
//! папки, обходя их рекурсивно. Точный список изменённых путей не нужен и
//! усложнил бы commit ради экономии одного прохода.

#![cfg_attr(not(target_os = "android"), allow(dead_code))]

use std::path::Path;

/// Попросить систему перечитать библиотеку. Ошибка не должна ломать commit:
/// файлы уже на диске, а скан можно повторить позже.
pub fn scan(root: &Path) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        android::scan(root)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = root;
        Ok(())
    }
}

#[cfg(target_os = "android")]
mod android {
    use super::*;
    use jni::objects::{JObject, JObjectArray, JString, JValue};
    use std::path::PathBuf;

    pub fn scan(root: &Path) -> Result<(), String> {
        // На Android контекст выставляет tao до запуска приложения. Если его
        // почему-то нет, это не повод падать из-за одного скана.
        let ctx = std::panic::catch_unwind(ndk_context::android_context)
            .map_err(|_| "контекст Android ещё не готов".to_string())?;
        let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }
            .map_err(|e| format!("JavaVM: {e}"))?;
        let mut env = vm
            .attach_current_thread()
            .map_err(|e| format!("поток JNI: {e}"))?;

        let path = PathBuf::from(root);
        let context = unsafe { JObject::from_raw(ctx.context().cast()) };
        let result = (|| -> Result<(), String> {
            let class = env
                .find_class("android/media/MediaScannerConnection")
                .map_err(|e| format!("MediaScannerConnection: {e}"))?;
            let paths: JObjectArray = env
                .new_object_array(1, "java/lang/String", JObject::null())
                .map_err(|e| format!("массив путей: {e}"))?;
            let text: JString = env
                .new_string(path.to_string_lossy())
                .map_err(|e| format!("путь для сканера: {e}"))?;
            env.set_object_array_element(&paths, 0, &text)
                .map_err(|e| format!("элемент массива путей: {e}"))?;

            // Пустые mime-типы: пусть определит сам. Слушатель не нужен.
            let mime = JObject::null();
            let listener = JObject::null();
            env.call_static_method(
                class,
                "scanFile",
                "(Landroid/content/Context;[Ljava/lang/String;[Ljava/lang/String;\
                 Landroid/media/MediaScannerConnection$OnScanCompletedListener;)V",
                &[
                    JValue::Object(&context),
                    JValue::Object(&paths),
                    JValue::Object(&mime),
                    JValue::Object(&listener),
                ],
            )
            .map_err(|e| format!("scanFile: {e}"))?;
            Ok(())
        })();

        if result.is_err() && env.exception_check().unwrap_or(false) {
            let _ = env.exception_describe();
            let _ = env.exception_clear();
        }
        result
    }
}
