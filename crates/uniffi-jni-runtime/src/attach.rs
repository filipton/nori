/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

use std::cell::RefCell;
use std::ffi::c_void;

use jni_sys::*;

/// NORI: detaches the thread when it ends. Android aborts when an attached thread exits; staying attached
/// until then makes later callbacks from the same thread free.
struct Detach(*mut JavaVM);

impl Drop for Detach {
    fn drop(&mut self) {
        // SAFETY: the JavaVM outlives every thread, and this thread attached itself.
        unsafe {
            ((**self.0).v1_2.DetachCurrentThread)(self.0);
        }
    }
}

thread_local! {
    static DETACH: RefCell<Option<Detach>> = const { RefCell::new(None) };
}

/// NORI: attaches under the Rust thread's name (the JVM otherwise renames it "Thread-NN").
unsafe fn attach_named(jvm: *mut JavaVM, penv: *mut *mut c_void) -> bool {
    let name = std::ffi::CString::new(std::thread::current().name().unwrap_or("nori")).unwrap_or_default();
    let mut args = JavaVMAttachArgs { version: JNI_VERSION_1_6, name: name.as_ptr() as *mut _, group: std::ptr::null_mut() };
    // SAFETY: the caller's JavaVM; `args` and `name` outlive the call.
    unsafe { ((**jvm).v1_2.AttachCurrentThread)(jvm, penv, std::ptr::from_mut(&mut args).cast()) == JNI_OK }
}

/// Registers this thread's detach-on-exit; false when its thread locals are already torn down.
fn detach_on_exit(jvm: *mut JavaVM) -> bool {
    DETACH.try_with(|d| *d.borrow_mut() = Some(Detach(jvm))).is_ok()
}

/// NORI: attaches this thread until it ends; returns whether it is attached.
///
/// # Safety
///
/// jvm must point to a valid JavaVM
pub unsafe fn attach_for_life(jvm: *mut JavaVM) -> bool {
    let mut env: *mut JNIEnv = ::std::ptr::null_mut();
    // SAFETY: documented JNI use on the caller's JavaVM.
    unsafe {
        let penv = std::ptr::from_mut(&mut env).cast::<*mut c_void>();
        match ((**jvm).v1_2.GetEnv)(jvm, penv, JNI_VERSION_1_2) {
            JNI_OK => true,
            JNI_EDETACHED => {
                if !attach_named(jvm, penv) {
                    return false;
                }
                if !detach_on_exit(jvm) {
                    ((**jvm).v1_2.DetachCurrentThread)(jvm);
                    return false;
                }
                true
            }
            _ => false,
        }
    }
}

/// Attach the current thread to the JVM and run a closure
///
/// This is used to implement callback interfaces.
///
/// The closure must not return any JNI objects,
/// since their lifetime ends before this function returns.
///
/// # Safety
///
/// jvm must point to a valid JavaVM
pub unsafe fn attach_current_thread<F, T>(jvm: *mut JavaVM, f: F) -> T
where
    F: FnOnce(*mut JNIEnv) -> T,
{
    let mut env: *mut JNIEnv = ::std::ptr::null_mut();
    // Safety:
    // We're using the JNI API correctly
    unsafe {
        let penv = std::ptr::from_mut(&mut env).cast::<*mut c_void>();
        // NORI: an already attached thread is used as is and never detached here; a thread attached here
        // stays attached until it ends, or detaches right after `f` if its thread locals are gone.
        let mut detach_after = false;
        match ((**jvm).v1_2.GetEnv)(jvm, penv, JNI_VERSION_1_2) {
            JNI_OK => {}
            JNI_EDETACHED => {
                if !attach_named(jvm, penv) {
                    panic!("AttachCurrentThread failed");
                }
                detach_after = !detach_on_exit(jvm);
            }
            _ => panic!("GetEnv failed"),
        }

        // Create a new local frame, needed to generate JNI references.
        // Create capacity for 16 references, which is what JNA does
        if ((**env).v1_2.PushLocalFrame)(env, 16) != 0 {
            panic!("Out of memory: PushLocalFrame failed");
        }
        let closure_result = f(env);
        ((**env).v1_2.PopLocalFrame)(env, std::ptr::null_mut());
        if detach_after {
            ((**jvm).v1_2.DetachCurrentThread)(jvm);
        }
        closure_result
    }
}
