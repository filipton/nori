//! `nori_core::heard` (which song is audible, and where) for the seek bar, called every frame:
//! primitives in, one packed `long` out, no allocation.

use jni::sys::{jboolean, jint, jlong};
use nori_core::heard::HeardClock;
use parking_lot::Mutex;

use crate::{native, Class};

pub(crate) static HEARD: Class = Class {
    name: c"dev/nori/music/playback/HeardJni",
    methods: &[native!(c"create", c"()J", create), native!(c"at", c"(JJZJ)J", at)],
};

pub(crate) static PLAYHEAD: Class = Class {
    name: c"dev/nori/music/playback/PlayheadJni",
    methods: &[
        native!(c"position", c"(JJZIIJIJ)J", position),
        native!(c"drifted", c"(JJ)Z", drifted),
        native!(c"runOn", c"(JJZ)J", run_on),
        native!(c"jumped", c"(J)V", jumped),
        native!(c"durationMs", c"(JJJ)J", duration_ms),
    ],
};

fn clock<'a>(h: jlong) -> Option<&'a Mutex<HeardClock>> {
    // SAFETY: a non-zero `h` is a pointer `create` made, which lives as long as the process.
    (h != 0).then(|| unsafe { &*(h as *const Mutex<HeardClock>) })
}

/// The process's clock (Kotlin keeps one for the app's life), never freed.
extern "system" fn create() -> jlong {
    Box::into_raw(Box::new(Mutex::new(HeardClock::new()))) as jlong
}

/// Returns `HeardAt::pack`.
extern "system" fn at(h: jlong, now_ms: jlong, playing: jboolean, position_ms: jlong) -> jlong {
    let Some(c) = clock(h) else { return position_ms.max(0) };
    c.lock().at(now_ms, playing != 0, position_ms).pack()
}

/// [`at`] for the seek bar of queue index `shown` (-1: none). `position_ms` is the controller's position,
/// `engine_ms` the engine's in song `on` (-1: none); the perf build checks the two agree.
#[allow(clippy::too_many_arguments)]
extern "system" fn position(h: jlong, now_ms: jlong, playing: jboolean, on: jint, next: jint, position_ms: jlong, shown: jint, engine_ms: jlong) -> jlong {
    let Some(c) = clock(h) else { return position_ms.max(0) };
    let at = c.lock().position(now_ms, playing != 0, usize::try_from(on).ok(), usize::try_from(next).ok(), position_ms, usize::try_from(shown).ok(), engine_ms);
    if nori_perf::invariants::on() {
        // Only on the player's own song: a page one song behind lags by design.
        let same = on >= 0 && on == shown && at.index.is_none();
        nori_perf::invariants::place_seen(now_ms, playing != 0 && same, at.ms, engine_ms, position_ms);
    }
    at.pack()
}

/// `nori_player::heard::drifted`: the session must republish its position.
extern "system" fn drifted(word_ms: jlong, engine_ms: jlong) -> jboolean {
    nori_player::heard::drifted(word_ms, engine_ms) as jboolean
}

/// A seek: the next reading is shown as is, even if earlier.
extern "system" fn jumped(h: jlong) {
    if let Some(c) = clock(h) {
        c.lock().jumped();
    }
}

extern "system" fn run_on(h: jlong, now_ms: jlong, playing: jboolean) -> jlong {
    clock(h).map_or(0, |c| c.lock().run_on(now_ms, playing != 0))
}

/// `nori_player::heard::shown_duration_ms`; `heard_s` -1: none.
extern "system" fn duration_ms(heard_s: jlong, player_ms: jlong, tagged_ms: jlong) -> jlong {
    nori_player::heard::shown_duration_ms((heard_s >= 0).then_some(heard_s), player_ms, tagged_ms)
}
