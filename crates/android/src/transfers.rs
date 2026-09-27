//! Downloads as media3 runs them (`nori_core::transfers`): the platform reports each song's state and
//! each chunk, and asks for the facts its notification and downloads screen are worded from. The
//! per-chunk report is primitives only; nothing is looked up by name or allocated while bytes flow.

use jni::objects::{JClass, JIntArray, JLongArray, JString};
use jni::sys::{jfloat, jint, jlong, jstring};
use jni::JNIEnv;
use nori_core::transfers::{self, NoticeKind, SummaryText, SummaryTitle};

use crate::{java_string, native, with_str, Class};

pub(crate) static DOWNLOADS: Class = Class {
    name: c"dev/nori/music/downloads/DownloadsJni",
    methods: &[
        native!(c"held", c"(Ljava/lang/String;)I", held),
        native!(c"followed", c"(Ljava/lang/String;IJ)I", followed),
        native!(c"removed", c"(Ljava/lang/String;)I", removed),
        native!(c"unmark", c"(Ljava/lang/String;)I", unmark),
        native!(c"startFraction", c"(Ljava/lang/String;)F", start_fraction),
        native!(c"open", c"(Ljava/lang/String;J)I", open),
        native!(c"note", c"(IJJJ)F", note),
        native!(c"notice", c"(IIJ)I", notice),
        native!(c"noticeFacts", c"([J)Ljava/lang/String;", notice_facts),
        native!(c"summary", c"([I)Ljava/lang/String;", summary),
    ],
};

pub(crate) static LINES: Class = Class {
    name: c"dev/nori/music/downloads/DownloadFacts",
    methods: &[
        native!(c"row", c"(Ljava/lang/String;[J)Ljava/lang/String;", row),
        native!(c"speedEta", c"([J)V", speed_eta),
    ],
};

/// Whether `id` is downloaded: 0 no, 1 queued or failed, 2 finished. Asked by every row a list draws.
extern "system" fn held(env: JNIEnv, _: JClass, id: JString) -> jint {
    with_str(&env, &id, transfers::held).unwrap_or(0)
}

/// media3 reported `id` in `state`; returns `transfers::followed`'s flags.
extern "system" fn followed(env: JNIEnv, _: JClass, id: JString, state: jint, now: jlong) -> jint {
    with_str(&env, &id, |id| transfers::followed(id, state, now)).unwrap_or(0)
}

extern "system" fn removed(env: JNIEnv, _: JClass, id: JString) -> jint {
    with_str(&env, &id, transfers::removed).unwrap_or(0)
}

extern "system" fn unmark(env: JNIEnv, _: JClass, id: JString) -> jint {
    with_str(&env, &id, transfers::unmark).unwrap_or(0)
}

extern "system" fn start_fraction(env: JNIEnv, _: JClass, id: JString) -> jfloat {
    with_str(&env, &id, transfers::start_fraction).unwrap_or(-1.0)
}

extern "system" fn open(env: JNIEnv, _: JClass, id: JString, now: jlong) -> jint {
    with_str(&env, &id, |id| transfers::open(id, now)).unwrap_or(-1)
}

/// A chunk arrived on `slot`; called per chunk, so primitives only.
extern "system" fn note(slot: jint, length: jlong, bytes: jlong, now: jlong) -> jfloat {
    transfers::note(slot, length, bytes, now)
}

extern "system" fn notice(listed: jint, waiting: jint, now: jlong) -> jint {
    transfers::notice(listed, waiting != 0, now)
}

/// The notification's facts as `notice` last found them: `out` gets `[kind, position, total, permille,
/// speed_bps, eta_s]` (kind as `NoticeKind`'s place: 0 waiting, 1 one song named, 2 one song, 3 more);
/// the song in flight's title and the batch's album come back as "title\nalbum". One crossing a second.
extern "system" fn notice_facts(env: JNIEnv, _: JClass, out: JLongArray) -> jstring {
    transfers::notice_facts(|n| {
        let kind = match n.kind {
            NoticeKind::Waiting => 0,
            NoticeKind::OneNamed => 1,
            NoticeKind::One => 2,
            NoticeKind::Many => 3,
        };
        let facts = [kind, n.position as jlong, n.total as jlong, n.permille as jlong, n.speed_bps, n.eta_s];
        if env.set_long_array_region(&out, 0, &facts).is_err() {
            return std::ptr::null_mut();
        }
        let mut names = String::with_capacity(n.current.len() + 1 + n.label.len());
        names.push_str(&n.current);
        names.push('\n');
        names.push_str(&n.label);
        java_string(&env, &names)
    })
}

/// How the batch went, once its bytes are in: `out` gets `[title, text, done, failed]` (title as
/// `SummaryTitle`'s place: 0 failed, 1 an album, 2 downloaded; text as `SummaryText`'s: 0 none, 1 some failed,
/// 2 try again) and the album comes back; null when there is nothing to say.
extern "system" fn summary(env: JNIEnv, _: JClass, out: JIntArray) -> jstring {
    let Some(s) = transfers::summary() else { return std::ptr::null_mut() };
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

/// A song's row, asked whenever its ring moves: its artist comes back, and `out` gets `[running, percent,
/// speed_bps, eta_s]` (running 0 leaves the rest as they were).
extern "system" fn row(env: JNIEnv, _: JClass, id: JString, out: JLongArray) -> jstring {
    with_str(&env, &id, |id| {
        transfers::row(id, |artist, facts| {
            let f = facts.map_or([0, 0, 0, 0], |f| [1, f.percent as jlong, f.speed_bps, f.eta_s]);
            if env.set_long_array_region(&out, 0, &f).is_err() {
                return std::ptr::null_mut();
            }
            java_string(&env, artist)
        })
    })
    .unwrap_or(std::ptr::null_mut())
}

/// The batch's bytes a second and seconds left, into `out`: the downloads screen's summary line, once a
/// second while it is open.
extern "system" fn speed_eta(env: JNIEnv, _: JClass, out: JLongArray) {
    let (speed, eta) = transfers::speed_eta();
    let _ = env.set_long_array_region(&out, 0, &[speed, eta]);
}
