//! JNI platform bridge. The activity is borrowed from AndroidApp, never leaked.
use crate::android_input::{ImeEvent, MAX_IME_BYTES};
use jni::objects::{JClass, JObject, JString};
use jni::refs::Global;
use jni::sys::{jboolean, jint};
use jni::{jni_sig, jni_str, EnvUnowned, JValue, JavaVM};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use winit::platform::android::activity::AndroidApp;

type Listener = Arc<dyn Fn(ImeEvent) + Send + Sync>;

struct Subscription {
    generation: u64,
    listener: Option<Listener>,
}

static SUBSCRIPTION: Mutex<Subscription> = Mutex::new(Subscription {
    generation: 0,
    listener: None,
});
static TRACE_INPUT: AtomicBool = AtomicBool::new(false);

pub(crate) struct AndroidIme {
    app: AndroidApp,
    generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AccessibleControl {
    pub id: i32,
    pub label: &'static str,
    pub bounds: [u32; 4],
    pub selected: bool,
    pub enabled: bool,
    pub toggle: bool,
}

impl AndroidIme {
    pub(crate) fn new(
        app: AndroidApp,
        callback: impl Fn(ImeEvent) + Send + Sync + 'static,
    ) -> Result<Self, String> {
        TRACE_INPUT.store(
            app.internal_data_path()
                .is_some_and(|path| path.join("config/kokuban/trace-frames").exists()),
            Ordering::Relaxed,
        );
        let generation = {
            let mut subscription = SUBSCRIPTION
                .lock()
                .map_err(|_| "IME subscription lock poisoned")?;
            subscription.generation = subscription.generation.wrapping_add(1);
            subscription.listener = Some(Arc::new(callback));
            subscription.generation
        };
        let ime = Self { app, generation };
        ime.with_activity(|env, activity| {
            env.call_method(
                activity,
                jni_str!("refreshInput"),
                jni_sig!(() -> void),
                &[],
            )?;
            Ok(())
        })?;
        Ok(ime)
    }

    pub(crate) fn set_visible(&self, visible: bool) -> Result<(), String> {
        self.with_activity(|env, activity| {
            env.call_method(
                activity,
                jni_str!("setKeyboardVisible"),
                jni_sig!((boolean) -> void),
                &[JValue::Bool(visible)],
            )?;
            Ok(())
        })
    }

    pub(crate) fn copy(&self, text: &str) -> Result<(), String> {
        if text.len() > MAX_IME_BYTES {
            return Err("Selection exceeds Android clipboard limit (64 KiB)".into());
        }
        self.with_activity(|env, activity| {
            let value = JString::from_str(env, text)?;
            env.call_method(
                activity,
                jni_str!("copyText"),
                jni_sig!((java.lang.String) -> void),
                &[JValue::Object(value.as_ref())],
            )?;
            Ok(())
        })
    }

    /// Clipboard access runs on the UI thread and returns an ImeEvent::Clipboard.
    pub(crate) fn paste(&self) -> Result<(), String> {
        self.with_activity(|env, activity| {
            env.call_method(activity, jni_str!("pasteText"), jni_sig!(() -> void), &[])?;
            Ok(())
        })
    }

    pub(crate) fn show_error(&self, message: &str) -> Result<(), String> {
        let message: String = message.chars().take(512).collect();
        self.with_activity(|env, activity| {
            let value = JString::from_str(env, message)?;
            env.call_method(
                activity,
                jni_str!("showError"),
                jni_sig!((java.lang.String) -> void),
                &[JValue::Object(value.as_ref())],
            )?;
            Ok(())
        })
    }

    pub(crate) fn update_controls(&self, controls: &[AccessibleControl]) -> Result<(), String> {
        if controls.len() > 32 {
            return Err("Too many accessibility controls".into());
        }
        self.with_activity(|env, activity| {
            let labels = env.new_object_array(
                controls.len() as i32,
                jni_str!("java/lang/String"),
                JObject::null(),
            )?;
            let mut data = Vec::with_capacity(controls.len() * 8);
            for (index, control) in controls.iter().enumerate() {
                let label = JString::from_str(env, control.label)?;
                labels.set_element(env, index, &label)?;
                data.push(control.id);
                data.extend(
                    control
                        .bounds
                        .map(|coordinate| coordinate.min(i32::MAX as u32) as i32),
                );
                data.extend([
                    i32::from(control.selected),
                    i32::from(control.enabled),
                    i32::from(control.toggle),
                ]);
            }
            let bounds = env.new_int_array(data.len())?;
            bounds.set_region(env, 0, &data)?;
            env.call_method(
                activity,
                jni_str!("updateControls"),
                jni_sig!("([Ljava/lang/String;[I)V"),
                &[
                    JValue::Object(labels.as_ref()),
                    JValue::Object(bounds.as_ref()),
                ],
            )?;
            Ok(())
        })
    }

    /// Package-manager controlled executable library directory, not writable app data.
    pub(crate) fn native_library_dir(&self) -> Result<PathBuf, String> {
        self.with_activity(|env, activity| {
            let result = env
                .call_method(
                    activity,
                    jni_str!("nativeLibraryDir"),
                    jni_sig!(() -> java.lang.String),
                    &[],
                )?
                .l()?;
            let path = env.as_cast::<JString>(&result)?.try_to_string(env)?;
            Ok(PathBuf::from(path))
        })
    }

    fn with_activity<T>(
        &self,
        callback: impl FnOnce(&mut jni::Env<'_>, &JObject<'_>) -> jni::errors::Result<T>,
    ) -> Result<T, String> {
        JavaVM::singleton()
            .map_err(|e| e.to_string())?
            .attach_current_thread(|env| -> jni::errors::Result<T> {
                let raw = self.app.activity_as_ptr() as jni::sys::jobject;
                // AndroidApp owns this global reference for the duration of this call.
                // as_cast_raw borrows it and must not delete it on return.
                let activity = unsafe { env.as_cast_raw::<Global<JObject>>(&raw)? };
                callback(env, activity.as_ref())
            })
            .map_err(|e| format!("Android input bridge: {e}"))
    }
}

impl Drop for AndroidIme {
    fn drop(&mut self) {
        if let Ok(mut subscription) = SUBSCRIPTION.lock() {
            if subscription.generation == self.generation {
                subscription.listener = None;
                TRACE_INPUT.store(false, Ordering::Relaxed);
            }
        }
    }
}

fn emit(event: ImeEvent) {
    let listener = SUBSCRIPTION.lock().ok().and_then(|s| s.listener.clone());
    if let Some(listener) = listener {
        listener(event);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kokuban_terminal_KokubanActivity_nativeText<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    kind: jint,
    text: JString<'local>,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let text = text.try_to_string(env)?;
        if text.len() <= MAX_IME_BYTES {
            if TRACE_INPUT.load(Ordering::Relaxed) {
                let operation = match kind {
                    0 => "commit",
                    1 => "preedit",
                    2 => "finish",
                    _ => "unknown",
                };
                log::info!(
                    "ime callback operation={operation} nonempty={}",
                    !text.is_empty()
                );
            }
            match kind {
                0 => emit(ImeEvent::Commit(text)),
                1 => emit(ImeEvent::Preedit(text)),
                2 => emit(ImeEvent::Finish),
                _ => {}
            }
        }
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>();
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kokuban_terminal_KokubanActivity_nativeDelete<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    before: jint,
    after: jint,
) {
    env.with_env(|_| -> jni::errors::Result<()> {
        if before >= 0 && after >= 0 {
            emit(ImeEvent::Delete {
                before: before as u32,
                after: after as u32,
            });
        }
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>();
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kokuban_terminal_KokubanActivity_nativeKey<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    code: jint,
    unicode: jint,
    meta: jint,
) {
    env.with_env(|_| -> jni::errors::Result<()> {
        emit(ImeEvent::Key {
            code,
            unicode: unicode as u32,
            meta,
        });
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>();
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kokuban_terminal_KokubanActivity_nativeViewport<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    left: jint,
    top: jint,
    right: jint,
    bottom: jint,
    keyboard: jboolean,
) {
    env.with_env(|_| -> jni::errors::Result<()> {
        emit(ImeEvent::Viewport {
            left: left.max(0) as u32,
            top: top.max(0) as u32,
            right: right.max(0) as u32,
            bottom: bottom.max(0) as u32,
            keyboard,
        });
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>();
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kokuban_terminal_KokubanActivity_nativeClipboard<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    text: JString<'local>,
) {
    env.with_env(|env| -> jni::errors::Result<()> {
        let text = text.try_to_string(env)?;
        if text.len() <= MAX_IME_BYTES {
            emit(ImeEvent::Clipboard(text));
        }
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>();
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_kokuban_terminal_KokubanActivity_nativeControl<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    id: jint,
    action: jint,
) {
    env.with_env(|_| -> jni::errors::Result<()> {
        emit(ImeEvent::Control { id, action });
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>();
}
