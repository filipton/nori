/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! NORI: class lookup from any thread. `FindClass` on a thread Rust started uses the system class loader,
//! which lacks the app's classes; the app's loader is captured in `JNI_OnLoad` and used as a fallback.

use std::ffi::{CStr, CString};
use std::sync::OnceLock;

use jni_sys::*;

struct AppLoader {
    loader: jobject,
    load_class: jmethodID,
}

// SAFETY: a global reference and a method ID, both valid on every thread.
unsafe impl Send for AppLoader {}
unsafe impl Sync for AppLoader {}

/// Global: `find_class` is called from upstream code that has no handle to pass it through.
static APP_LOADER: OnceLock<AppLoader> = OnceLock::new();

/// Keeps the class loader of `class_name` for [`find_class`]. Returns false if it could not be found.
///
/// # Safety
///
/// env must point to a valid JNIEnv, on a thread whose `FindClass` sees the app's classes (`JNI_OnLoad`).
pub unsafe fn remember_class_loader(env: *mut JNIEnv, class_name: &CStr) -> bool {
    unsafe {
        let jni = &(**env).v1_4;
        let class = (jni.FindClass)(env, class_name.as_ptr());
        if class.is_null() {
            (jni.ExceptionClear)(env);
            return false;
        }
        let class_class = (jni.GetObjectClass)(env, class);
        let get_loader = (jni.GetMethodID)(env, class_class, c"getClassLoader".as_ptr(), c"()Ljava/lang/ClassLoader;".as_ptr());
        let loader = if get_loader.is_null() { std::ptr::null_mut() } else { (jni.CallObjectMethodA)(env, class, get_loader, std::ptr::null()) };
        let loader_class = (jni.FindClass)(env, c"java/lang/ClassLoader".as_ptr());
        let load_class = if loader_class.is_null() {
            std::ptr::null_mut()
        } else {
            (jni.GetMethodID)(env, loader_class, c"loadClass".as_ptr(), c"(Ljava/lang/String;)Ljava/lang/Class;".as_ptr())
        };
        if (jni.ExceptionCheck)(env) != JNI_FALSE || loader.is_null() || load_class.is_null() {
            (jni.ExceptionClear)(env);
            return false;
        }
        let loader_ref = (jni.NewGlobalRef)(env, loader);
        if APP_LOADER.set(AppLoader { loader: loader_ref, load_class }).is_err() {
            // Loaded twice into one process: the first loader stays.
            (jni.DeleteGlobalRef)(env, loader_ref);
        }
        for local in [class, class_class, loader, loader_class] {
            (jni.DeleteLocalRef)(env, local);
        }
        true
    }
}

/// `FindClass`, falling back on the loader [`remember_class_loader`] kept. Returns a local reference, or
/// null with an exception pending if neither knows the class.
///
/// # Safety
///
/// env must point to a valid JNIEnv
pub unsafe fn find_class(env: *mut JNIEnv, class_name: &CStr) -> jclass {
    unsafe {
        let jni = &(**env).v1_4;
        let class = (jni.FindClass)(env, class_name.as_ptr());
        if !class.is_null() {
            return class;
        }
        let Some(app) = APP_LOADER.get() else {
            return class;
        };
        (jni.ExceptionClear)(env);
        // `ClassLoader.loadClass` takes the binary name, with dots where `FindClass` has slashes.
        let dotted = CString::new(class_name.to_bytes().iter().map(|&b| if b == b'/' { b'.' } else { b }).collect::<Vec<u8>>())
            .expect("a CStr has no NUL inside");
        let name = (jni.NewStringUTF)(env, dotted.as_ptr());
        if name.is_null() {
            return std::ptr::null_mut();
        }
        let class = (jni.CallObjectMethodA)(env, app.loader, app.load_class, [jvalue { l: name }].as_ptr());
        (jni.DeleteLocalRef)(env, name);
        if (jni.ExceptionCheck)(env) != JNI_FALSE {
            return std::ptr::null_mut();
        }
        class
    }
}
