//! `nori_core::transfers` for media3's downloads: state and per-chunk progress in, notification and
//! downloads screen facts out. Per-chunk calls are primitives only. Kotlin holds the downloads of the
//! core in use as a handle ([`attach`]).

use std::sync::Arc;

use jni::objects::{JClass, JIntArray, JLongArray, JString};
use jni::sys::{jfloat, jint, jlong, jstring};
use jni::JNIEnv;
use nori_core::transfers::{Downloads, Notice, NoticeKind, SummaryText, SummaryTitle, Tracker};
use nori_core::Core;

use crate::{java_string, native, with_str, Class, Handles};

pub(crate) static DOWNLOADS: Class = Class {
    name: c"dev/nori/music/downloads/DownloadsJni",
    methods: &[
        native!(c"attach", c"(J)J", attach),
        native!(c"release", c"(J)V", release),
        native!(c"held", c"(JLjava/lang/String;)I", held),
        native!(c"followed", c"(JLjava/lang/String;IJ)I", followed),
        native!(c"removed", c"(JLjava/lang/String;)I", removed),
        native!(c"unmark", c"(JLjava/lang/String;)I", unmark),
        native!(c"startFraction", c"(JLjava/lang/String;)F", start_fraction),
        native!(c"open", c"(JLjava/lang/String;J)I", open),
        native!(c"note", c"(JIJJJ)F", note),
        native!(c"notice", c"(JIIJ)I", notice),
        native!(c"noticeFacts", c"(J[J)Ljava/lang/String;", notice_facts),
        native!(c"summary", c"(J[I)Ljava/lang/String;", summary),
    ],
};

pub(crate) static LINES: Class = Class {
    name: c"dev/nori/music/downloads/DownloadFacts",
    methods: &[
        native!(c"row", c"(JLjava/lang/String;[J)Ljava/lang/String;", row),
        native!(c"speedEta", c"(J[J)V", speed_eta),
    ],
};

/// The downloads Kotlin holds. A handle table: a download report racing a profile switch finds nothing.
static ATTACHED: Handles<Downloads> = Handles::new();

/// Runs `f` on the tracker of the downloads behind `h`; None for a handle let go.
fn with<R>(h: jlong, f: impl FnOnce(&mut Tracker) -> R) -> Option<R> {
    ATTACHED.get(h).map(|d| d.with(f))
}

/// The downloads of `core` (a `Core.uniffiCloneHandle()`, taken over) as a handle for the doors below.
extern "system" fn attach(_: JNIEnv, _: JClass, core: jlong) -> jlong {
    // SAFETY: Kotlin passes `Core.uniffiCloneHandle()`, once.
    let core: Arc<Core> = unsafe { crate::uniffi_object(core) };
    ATTACHED.add(core.transfers_arc())
}

/// Lets the downloads behind `h` go; whoever waits for their marks to move is woken.
extern "system" fn release(_: JNIEnv, _: JClass, h: jlong) {
    if let Some(d) = ATTACHED.remove(h) {
        d.let_go();
    }
}

/// 0 not downloaded, 1 queued or failed, 2 finished. Called per list row.
extern "system" fn held(env: JNIEnv, _: JClass, h: jlong, id: JString) -> jint {
    let Some(d) = ATTACHED.get(h) else { return 0 };
    with_str(&env, &id, |id| d.held().state(id).code()).unwrap_or(0)
}

/// media3 reported `id` in `state`; returns `Tracker::followed`'s flags.
extern "system" fn followed(env: JNIEnv, _: JClass, h: jlong, id: JString, state: jint, now: jlong) -> jint {
    with_str(&env, &id, |id| with(h, |t| t.followed(id, state, now))).flatten().unwrap_or(0)
}

extern "system" fn removed(env: JNIEnv, _: JClass, h: jlong, id: JString) -> jint {
    with_str(&env, &id, |id| with(h, |t| t.removed(id))).flatten().unwrap_or(0)
}

extern "system" fn unmark(env: JNIEnv, _: JClass, h: jlong, id: JString) -> jint {
    with_str(&env, &id, |id| with(h, |t| t.forget(id))).flatten().unwrap_or(0)
}

extern "system" fn start_fraction(env: JNIEnv, _: JClass, h: jlong, id: JString) -> jfloat {
    with_str(&env, &id, |id| with(h, |t| t.start_fraction(id))).flatten().unwrap_or(-1.0)
}

extern "system" fn open(env: JNIEnv, _: JClass, h: jlong, id: JString, now: jlong) -> jint {
    with_str(&env, &id, |id| with(h, |t| t.open(id, now))).flatten().unwrap_or(-1)
}

/// A chunk arrived on `slot`.
extern "system" fn note(h: jlong, slot: jint, length: jlong, bytes: jlong, now: jlong) -> jfloat {
    with(h, |t| t.note(slot, length, bytes, now)).unwrap_or(f32::NAN)
}

extern "system" fn notice(h: jlong, listed: jint, waiting: jint, now: jlong) -> jint {
    with(h, |t| t.notice(listed, waiting != 0, now)).unwrap_or(2)
}

/// The notification's facts: `out` gets `[kind, position, total, permille, speed_bps, eta_s]` (kind: the
/// `NoticeKind` ordinal); returns "title\nalbum".
extern "system" fn notice_facts(env: JNIEnv, _: JClass, h: jlong, out: JLongArray) -> jstring {
    // Copied out first: JNI calls under the tracker's lock can stall a collection that waits on `note`.
    let words = |n: &Notice| {
        let kind = match n.kind {
            NoticeKind::Waiting => 0,
            NoticeKind::OneNamed => 1,
            NoticeKind::One => 2,
            NoticeKind::Many => 3,
        };
        let facts = [kind, n.position as jlong, n.total as jlong, n.permille as jlong, n.speed_bps, n.eta_s];
        (facts, format!("{}\n{}", n.current, n.label))
    };
    let (facts, names) = with(h, |t| words(t.notice_facts())).unwrap_or_else(|| words(&Notice::default()));
    if env.set_long_array_region(&out, 0, &facts).is_err() {
        return std::ptr::null_mut();
    }
    java_string(&env, &names)
}

/// The finished batch: `out` gets `[title, text, done, failed]` (`SummaryTitle`/`SummaryText` ordinals);
/// returns the album, or null when there is no summary.
extern "system" fn summary(env: JNIEnv, _: JClass, h: jlong, out: JIntArray) -> jstring {
    let Some(s) = with(h, |t| t.summary()).flatten() else { return std::ptr::null_mut() };
    let title = match s.title {
        SummaryTitle::Failed => 0,
        SummaryTitle::Album => 1,
        SummaryTitle::Downloaded => 2,
    };
    let text = match s.text {
        SummaryText::None => 0,
        SummaryText::SomeFailed => 1,
        SummaryText::TryAgain => 2,
    };
    if env.set_int_array_region(&out, 0, &[title, text, s.done, s.failed]).is_err() {
        return std::ptr::null_mut();
    }
    java_string(&env, &s.label)
}

/// A song's row: returns its artist; `out` gets `[running, percent, speed_bps, eta_s]`.
extern "system" fn row(env: JNIEnv, _: JClass, h: jlong, id: JString, out: JLongArray) -> jstring {
    // Copied out first, as in `notice_facts`.
    let row = |id: &str| {
        with(h, |t| {
            let (artist, facts) = t.row(id);
            (facts, artist.to_string())
        })
        .unwrap_or_default()
    };
    let Some((facts, artist)) = with_str(&env, &id, row) else {
        return std::ptr::null_mut();
    };
    let f = facts.map_or([0, 0, 0, 0], |f| [1, f.percent as jlong, f.speed_bps, f.eta_s]);
    if env.set_long_array_region(&out, 0, &f).is_err() {
        return std::ptr::null_mut();
    }
    java_string(&env, &artist)
}

/// The batch's speed (bytes/s) and seconds left, into `out`.
extern "system" fn speed_eta(env: JNIEnv, _: JClass, h: jlong, out: JLongArray) {
    let (speed, eta) = with(h, Tracker::speed_eta).unwrap_or((0, -1));
    let _ = env.set_long_array_region(&out, 0, &[speed, eta]);
}
