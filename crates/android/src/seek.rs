//! `nori_player::seek::SeekKeeper` (re-issuing seeks the player dropped) for Kotlin.

use jni::sys::{jboolean, jlong};
use nori_player::seek::{SeekKeeper, Verdict};
use parking_lot::Mutex;

use crate::{native, Class};

pub(crate) static CLASS: Class = Class {
    name: c"dev/nori/music/playback/SeekJni",
    methods: &[
        native!(c"create", c"()J", create),
        native!(c"ask", c"(JJJZJ)V", ask),
        native!(c"forget", c"(J)V", forget),
        native!(c"look", c"(JJZZJZ)J", look),
    ],
};

fn keeper<'a>(h: jlong) -> Option<&'a Mutex<SeekKeeper>> {
    // SAFETY: a non-zero `h` came from `create` and is never freed.
    (h != 0).then(|| unsafe { &*(h as *const Mutex<SeekKeeper>) })
}

/// `look` results besides a position (>= 0) to seek to again.
const WATCH: jlong = -1;
const FORGET: jlong = -2;

extern "system" fn create() -> jlong {
    Box::into_raw(Box::new(Mutex::new(SeekKeeper::new()))) as jlong
}

extern "system" fn ask(h: jlong, target: jlong, now: jlong, ready: jboolean, pos: jlong) {
    if let Some(k) = keeper(h) {
        k.lock().ask(target, now, ready != 0, pos);
    }
}

extern "system" fn forget(h: jlong) {
    if let Some(k) = keeper(h) {
        k.lock().forget();
    }
}

/// [`WATCH`], [`FORGET`], or the position to seek to again.
extern "system" fn look(h: jlong, now: jlong, same_song: jboolean, ready: jboolean, pos: jlong, playing: jboolean) -> jlong {
    let Some(k) = keeper(h) else { return FORGET };
    match k.lock().look(now, same_song != 0, ready != 0, pos, playing != 0) {
        Verdict::Watch => WATCH,
        Verdict::Forget => FORGET,
        Verdict::SeekAgain(at) => at.max(0),
    }
}
