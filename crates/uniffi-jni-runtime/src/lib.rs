/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Upstream's uniffi Kotlin JNI runtime, re-exported, with the modules carrying nori's Android changes
//! (marked NORI) replaced:
//! - caching.rs: classes looked up through [`find_class`] instead of `FindClass`;
//! - attach.rs: threads attached here are detached when they end; `attach_for_life`;
//! - loader.rs: the app's class loader, for lookups from threads Rust started.

pub use uniffi_bindgen_kotlin_jni_runtime::*;

mod attach;
mod caching;
mod loader;

pub use attach::{attach_current_thread, attach_for_life};
pub use caching::{CachedClass, CachedMethod, CachedStaticMethod};
pub use loader::{find_class, remember_class_loader};
