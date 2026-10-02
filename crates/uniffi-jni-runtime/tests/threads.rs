//! NORI changes against a fake JVM: `FindClass` finds app classes only on "app threads" (as on Android),
//! and attaches/detaches are counted.

use std::ffi::{c_char, c_void, CStr, CString};
use std::mem::MaybeUninit;
use std::ptr::{addr_of_mut, null_mut};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use nori_uniffi_jni_runtime::*;

const CLASS: jclass = 0x10 as jclass;
const CLASS_CLASS: jclass = 0x20 as jclass;
const LOADER: jobject = 0x30 as jobject;
const GET_LOADER: jmethodID = 0x40 as jmethodID;
const LOAD_CLASS: jmethodID = 0x50 as jmethodID;
const STATIC_METHOD: jmethodID = 0x60 as jmethodID;

thread_local! {
    /// `FindClass` sees app classes on this thread (a Java thread, or `JNI_OnLoad`).
    static APP_THREAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static ATTACHED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

static LOADED: Mutex<Vec<String>> = Mutex::new(Vec::new());
static ATTACHES: AtomicUsize = AtomicUsize::new(0);
static DETACHES: AtomicUsize = AtomicUsize::new(0);

unsafe extern "system" fn find_class_fake(_: *mut JNIEnv, name: *const c_char) -> jclass {
    let name = unsafe { CStr::from_ptr(name) }.to_str().unwrap();
    if name.starts_with("java/") || APP_THREAD.get() {
        CLASS
    } else {
        null_mut()
    }
}
unsafe extern "system" fn get_object_class(_: *mut JNIEnv, _: jobject) -> jclass {
    CLASS_CLASS
}
unsafe extern "system" fn get_method_id(_: *mut JNIEnv, _: jclass, name: *const c_char, _: *const c_char) -> jmethodID {
    match unsafe { CStr::from_ptr(name) }.to_bytes() {
        b"getClassLoader" => GET_LOADER,
        b"loadClass" => LOAD_CLASS,
        _ => STATIC_METHOD,
    }
}
unsafe extern "system" fn get_static_method_id(_: *mut JNIEnv, _: jclass, _: *const c_char, _: *const c_char) -> jmethodID {
    STATIC_METHOD
}
unsafe extern "system" fn call_object_method(_: *mut JNIEnv, _: jobject, method: jmethodID, args: *const jvalue) -> jobject {
    if method == GET_LOADER {
        return LOADER;
    }
    let name = unsafe { CStr::from_ptr((*args).l as *const c_char) };
    LOADED.lock().unwrap().push(name.to_str().unwrap().to_string());
    CLASS
}
unsafe extern "system" fn new_string_utf(_: *mut JNIEnv, s: *const c_char) -> jstring {
    CString::from(unsafe { CStr::from_ptr(s) }).into_raw() as jstring
}
unsafe extern "system" fn new_global_ref(_: *mut JNIEnv, o: jobject) -> jobject {
    o
}
unsafe extern "system" fn delete_ref(_: *mut JNIEnv, _: jobject) {}
unsafe extern "system" fn exception_clear(_: *mut JNIEnv) {}
unsafe extern "system" fn exception_check(_: *mut JNIEnv) -> jboolean {
    JNI_FALSE
}
unsafe extern "system" fn push_local_frame(_: *mut JNIEnv, _: jint) -> jint {
    0
}
unsafe extern "system" fn pop_local_frame(_: *mut JNIEnv, _: jobject) -> jobject {
    null_mut()
}

unsafe extern "system" fn get_env(_: *mut JavaVM, penv: *mut *mut c_void, _: jint) -> jint {
    if !ATTACHED.get() {
        return JNI_EDETACHED;
    }
    unsafe { *penv = env().cast() };
    JNI_OK
}
unsafe extern "system" fn attach(_: *mut JavaVM, penv: *mut *mut c_void, _: *mut c_void) -> jint {
    ATTACHED.set(true);
    ATTACHES.fetch_add(1, Ordering::SeqCst);
    unsafe { *penv = env().cast() };
    JNI_OK
}
unsafe extern "system" fn detach(_: *mut JavaVM) -> jint {
    ATTACHED.set(false);
    DETACHES.fetch_add(1, Ordering::SeqCst);
    JNI_OK
}

/// A JNIEnv with only the entries the runtime calls filled in.
fn env() -> *mut JNIEnv {
    static ENV: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *ENV.get_or_init(|| {
        let table: &mut MaybeUninit<JNINativeInterface_> = Box::leak(Box::new(MaybeUninit::zeroed()));
        let t = table.as_mut_ptr();
        unsafe {
            addr_of_mut!((*t).v1_4.FindClass).write(find_class_fake);
            addr_of_mut!((*t).v1_4.GetObjectClass).write(get_object_class);
            addr_of_mut!((*t).v1_4.GetMethodID).write(get_method_id);
            addr_of_mut!((*t).v1_4.GetStaticMethodID).write(get_static_method_id);
            addr_of_mut!((*t).v1_4.CallObjectMethodA).write(call_object_method);
            addr_of_mut!((*t).v1_4.NewStringUTF).write(new_string_utf);
            addr_of_mut!((*t).v1_4.NewGlobalRef).write(new_global_ref);
            addr_of_mut!((*t).v1_4.DeleteLocalRef).write(delete_ref);
            addr_of_mut!((*t).v1_4.DeleteGlobalRef).write(delete_ref);
            addr_of_mut!((*t).v1_4.ExceptionClear).write(exception_clear);
            addr_of_mut!((*t).v1_4.ExceptionCheck).write(exception_check);
            addr_of_mut!((*t).v1_4.PushLocalFrame).write(push_local_frame);
            addr_of_mut!((*t).v1_4.PopLocalFrame).write(pop_local_frame);
        }
        let env: &mut JNIEnv = Box::leak(Box::new(t as JNIEnv));
        env as *mut JNIEnv as usize
    }) as *mut JNIEnv
}

fn vm() -> *mut JavaVM {
    static VM: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *VM.get_or_init(|| {
        let table: &mut MaybeUninit<JNIInvokeInterface_> = Box::leak(Box::new(MaybeUninit::zeroed()));
        let t = table.as_mut_ptr();
        unsafe {
            addr_of_mut!((*t).v1_4.GetEnv).write(get_env);
            addr_of_mut!((*t).v1_4.AttachCurrentThread).write(attach);
            addr_of_mut!((*t).v1_4.DetachCurrentThread).write(detach);
        }
        let vm: &mut JavaVM = Box::leak(Box::new(t as JavaVM));
        vm as *mut JavaVM as usize
    }) as *mut JavaVM
}

#[test]
fn rust_thread_finds_app_class_through_saved_loader() {
    APP_THREAD.set(true); // as in JNI_OnLoad
    assert!(unsafe { remember_class_loader(env(), c"uniffi/UniffiKt") });
    APP_THREAD.set(false);

    let found = std::thread::spawn(|| unsafe {
        let class = find_class(env(), c"uniffi/UniffiKt");
        // A callback's lookup; used to panic with "Class not found".
        static METHOD: CachedStaticMethod = CachedStaticMethod::new(c"uniffi/UniffiKt", c"uniffiContinuationResume", c"(Lkotlin/coroutines/Continuation;)V");
        let (cached, method) = METHOD.get(env());
        (class as usize, cached as usize, method as usize)
    })
    .join()
    .unwrap();
    assert_eq!(found, (CLASS as usize, CLASS as usize, STATIC_METHOD as usize));
    assert_eq!(*LOADED.lock().unwrap(), ["uniffi.UniffiKt", "uniffi.UniffiKt"], "asked of the app's loader by binary name");
}

#[test]
fn attaches_once_and_detaches_at_thread_exit() {
    let before = (ATTACHES.load(Ordering::SeqCst), DETACHES.load(Ordering::SeqCst));
    std::thread::spawn(move || unsafe {
        attach_current_thread(vm(), |_| ());
        attach_current_thread(vm(), |_| ());
        assert_eq!((ATTACHES.load(Ordering::SeqCst), DETACHES.load(Ordering::SeqCst)), (before.0 + 1, before.1));
    })
    .join()
    .unwrap();
    assert_eq!((ATTACHES.load(Ordering::SeqCst), DETACHES.load(Ordering::SeqCst)), (before.0 + 1, before.1 + 1));

    // An already attached (Java) thread is neither attached nor detached.
    std::thread::spawn(|| unsafe {
        ATTACHED.set(true);
        attach_current_thread(vm(), |_| ());
    })
    .join()
    .unwrap();
    assert_eq!((ATTACHES.load(Ordering::SeqCst), DETACHES.load(Ordering::SeqCst)), (before.0 + 1, before.1 + 1));
}
