// The C ABI of libnori_ios.a (crates/ios/src/lib.rs, crates/ios/src/session.rs). Imported into Swift
// as the bridging header (tools/ipod.sh: `-import-objc-header`). Keep the two in step by hand.

#ifndef NORI_IOS_H
#define NORI_IOS_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/// The workspace version; a static string, never freed.
const char *nori_ios_version(void);

/// Opens the app database under `data_dir` and answers a short English note (milestone 1's screen).
/// Free with `nori_ios_free`; NULL for an unreadable path.
char *nori_ios_probe(const char *data_dir);

/// Frees a string this library handed out.
void nori_ios_free(char *s);

/// One session report. `id` and `text` are valid only for the duration of the callback.
/// `kind`: 1 state, 2 song, 3 looped, 4 position, 5 error, 6 output, 7 buffering, 8 stopped,
/// 9 note, 10 reachable, 11 lyrics, 12 search, 13 title, 14 mixing, 15 bridge, 16 placed, 17 awake.
/// `state`: 0 idle, 1 playing, 2 paused, 3 ended.
typedef struct NoriReport {
    int32_t kind;
    int32_t state;
    int32_t index;
    int64_t ms;
    uint64_t jumps;
    int32_t flag;
    const char *id;
    const char *text;
} NoriReport;

typedef void (*NoriReportFn)(const NoriReport *report);

/// Where reports go, delivered on a thread the library owns. NULL clears it. The callback may call
/// the controls below.
void nori_ios_on_report(NoriReportFn cb);

/// Opens saved server `server_id` (empty: the active one) and the playback session. NULL on success;
/// otherwise an English error to free with `nori_ios_free`. A failure leaves no session open.
char *nori_ios_open(const char *data_dir, const char *server_id);
/// Keeps the core's log in `dir`/nori.log, local times at `utc_offset_min`.
void nori_ios_keep_log(const char *dir, int32_t utc_offset_min);

/// Plays queue index `index` from `ms`. The jump number, or 0 when nothing is open.
uint64_t nori_ios_play_at(int32_t index, int64_t ms);

void nori_ios_toggle(void);
void nori_ios_next(void);
void nori_ios_previous(void);
void nori_ios_seek(int64_t ms);

/// Goes to `index` at `ms`, playing or paused as before. The jump number, or 0 when nothing is open.
uint64_t nori_ios_go_to(int32_t index, int64_t ms);

/// Repeat off (0), one (1) or all (2).
void nori_ios_set_repeat(int32_t mode);

/// `on` is 0 or 1.
void nori_ios_shuffle(int32_t on);

void nori_ios_remove(int32_t index);
void nori_ios_clear_upcoming(void);

/// Puts back a removed song. `id` is NUL-terminated.
void nori_ios_put_back(const char *id);

void nori_ios_move(int32_t from, int32_t to);

/// Saves the queue now (the app left the foreground). Does not push it.
void nori_ios_background(void);

/// Drops decoded covers.
void nori_ios_memory_warning(void);

/// Closes the open session and opens the active saved server: NULL, or an error to free.
char *nori_ios_reopen(const char *data_dir);

/// The saved servers as JSON to free: [{id, label, url, active}].
char *nori_ios_servers(const char *data_dir);
/// Makes server `id` the active one: 1, or 0 for an unknown id. Takes effect at nori_ios_reopen.
int32_t nori_ios_pick_server(const char *data_dir, const char *id);
void nori_ios_remove_server(const char *data_dir, const char *id);

/// 1 when a playback session is open.
int32_t nori_ios_is_open(void);

/// The system volume moved (0 to 1): loudness compensation follows it.
void nori_ios_volume(float fraction);

/// Note report codes (`flag`; `index` is the count where there is one).
#define NORI_NOTE_QUEUED_NEXT 1
#define NORI_NOTE_QUEUED_LAST 2
#define NORI_NOTE_NOTHING_TO_PLAY 3
#define NORI_NOTE_SONGS_FAILED 4
#define NORI_NOTE_NOTHING_TO_PUT_BACK 5
#define NORI_NOTE_DOWNLOADING 6
#define NORI_NOTE_DOWNLOAD_FAILED 7
#define NORI_NOTE_STARRED 8
#define NORI_NOTE_UNSTARRED 9
#define NORI_NOTE_STAR_FAILED 10
#define NORI_NOTE_INDEXING 11
#define NORI_NOTE_INDEXED 12
#define NORI_NOTE_INDEX_STOPPED 13
#define NORI_NOTE_DONE 14
#define NORI_NOTE_FORGOT 15

/// Pages for `nori_ios_read`. `arg`: the id for album/artist/playlist/mix, the name for a genre, the
/// query for search, the offset for songs; empty otherwise.
#define NORI_PAGE_HOME 1
#define NORI_PAGE_ALBUMS 2
#define NORI_PAGE_ARTISTS 3
#define NORI_PAGE_PLAYLISTS 4
#define NORI_PAGE_SONGS 5
#define NORI_PAGE_GENRES 6
#define NORI_PAGE_DOWNLOADS 7
#define NORI_PAGE_ALBUM 8
#define NORI_PAGE_ARTIST 9
#define NORI_PAGE_PLAYLIST 10
#define NORI_PAGE_GENRE 11
#define NORI_PAGE_QUEUE 12
#define NORI_PAGE_SEARCH 13
#define NORI_PAGE_STARRED 14
#define NORI_PAGE_MIX 15
#define NORI_PAGE_SMARTS 16
#define NORI_PAGE_SMART 17

/// A page answer: `json` is valid for the call only. Comes on a library thread, the stored copy first.
typedef void (*NoriPageFn)(uint64_t token, const char *json);
void nori_ios_on_page(NoriPageFn cb);
void nori_ios_read(uint64_t token, int32_t kind, const char *arg);

/// Taps on a page's songs, by the token it was read with.
void nori_ios_play_list(uint64_t token, int32_t index, int32_t shuffle);
/// `index` -1 for the whole list.
void nori_ios_enqueue_list(uint64_t token, int32_t index, int32_t next);
void nori_ios_download_list(uint64_t token, int32_t index);

/// Album (8), artist (9) or playlist (10) by id, fetched first.
void nori_ios_play_collection(int32_t kind, const char *id, int32_t shuffle);
void nori_ios_enqueue_collection(int32_t kind, const char *id, int32_t next);
void nori_ios_download_collection(int32_t kind, const char *id);

/// Song 1, album 2, artist 3.
void nori_ios_star(int32_t kind, const char *id, int32_t on);

/// What plays now, as JSON; free it. NULL when nothing is open.
char *nori_ios_now(void);

/// A decoded cover: straight RGBA, `len` bytes, valid until `owner` is given to nori_ios_cover_release
/// (so it is drawn without a copy). Comes on a loader thread.
typedef void (*NoriCoverFn)(uint64_t token, uint32_t width, uint32_t height, const uint8_t *rgba, size_t len, void *owner);
void nori_ios_cover_release(void *owner);
void nori_ios_on_cover(NoriCoverFn cb);
void nori_ios_cover(uint64_t token, const char *id, uint32_t px);
void nori_ios_cover_cancel(uint64_t token);

/// Asks for lyrics; a lyrics report says when an answer came, then read it as JSON (free it).
void nori_ios_lyrics(const char *song);
char *nori_ios_lyrics_page(const char *song);

/// The lyrics page's clock (crates/ios/src/lyrics.rs): which line is lit, how far it is sung, and when
/// to ask again. A handle on the lyrics held for a song; NULL when none are held.
typedef struct NoriLyricStep {
    int32_t active;   // lit line; -1 before the first or unsynced; the line count once over
    float sung;       // fill in the lit line, fractional UTF-16 units
    int32_t glide_ms;
    uint32_t wait;    // next call: display frames while filling (still 0), else ms; 0 never
    int32_t still;
    int32_t redraw;
} NoriLyricStep;
typedef struct LyricClock LyricClock;
LyricClock *nori_ios_lyric_clock(const char *song, int64_t position_ms);
int32_t nori_ios_lyric_sweeps(LyricClock *clock);
void nori_ios_lyric_advance(LyricClock *clock, int64_t position_ms, int32_t sweep, int32_t force, NoriLyricStep *out);
/// A tap on `line`: lit at once; the position to seek to.
int64_t nori_ios_lyric_tap(LyricClock *clock, int32_t line);
void nori_ios_lyric_free(LyricClock *clock);
float nori_ios_lyric_strength(int32_t synced, int32_t line, int32_t active);
float nori_ios_lyric_unsung(void);

/// The settings with kinds, options and values, as JSON (free it); set one by name (1 if changed).
/* A long list's orders, JSON {now, all} to free; NULL for a page without orders. */
char *nori_ios_sorts(int32_t kind);
/* Keeps one of nori_ios_sorts' `all` as the list's order. */
void nori_ios_sort(int32_t kind, const char *name);
/* The built-in equalizer curves by PresetKind code, a JSON array to free. */
char *nori_ios_presets(void);
/* Applies curve `index` of nori_ios_presets; 1 when the sound changed. */
int32_t nori_ios_preset(uint32_t index);
char *nori_ios_settings(void);
int32_t nori_ios_set(const char *name, const char *value);

/// The graphic equalizer as JSON (free it); move one band, getting the gain kept.
char *nori_ios_equalizer(void);
float nori_ios_equalizer_band(uint32_t index, float gain_db);
/// The equalizer screen is in sight and was touched: the short output buffer follows the core's rule.
void nori_ios_tuning(int32_t in_sight, int32_t touched);

/// Outputs and their sound (crates/ios/src/sound.rs), JSON to free:
/// {current, rows: [{output, port, name, current, choice, profile}]}.
/// `port`: 0 speaker, 1 wired, 2 USB, 3 Bluetooth, 4 other. `choice`: 0 automatic, 1 quiet, 2 flat,
/// 3 profile, 4 no processing.
char *nori_ios_devices(void);
/// One output's choices, JSON to free: {port, profiles, forget, curves: [{name, source, form, target, path}]}.
char *nori_ios_device_sheet(const char *output);
/// `profile` names a saved profile for choice 3. 1 when kept.
int32_t nori_ios_device_assign(const char *output, int32_t choice, const char *profile);
/// Saves an AutoEQ curve as `output`'s sound. Blocks on the network. 1 kept, 0 no curve, -1 failed.
int32_t nori_ios_device_adopt(const char *output, const char *name, const char *source,
                              const char *form, const char *target, const char *path);
void nori_ios_device_forget(const char *output);

/// The AutoEQ list's hits, JSON to free: {short, count, hits: [{name, source, form, target, path}]}.
char *nori_ios_autoeq_browse(const char *query);
/// Downloads the AutoEQ list. Blocks on the network. The headphones kept, or -1.
int32_t nori_ios_autoeq_update(void);
/// Makes an AutoEQ curve the current sound. Blocks on the network. 1 applied, 0 no curve, -1 failed.
int32_t nori_ios_autoeq_apply(const char *name, const char *source, const char *form,
                              const char *target, const char *path);

/// The song menu (crates/ios/src/menu.rs). A song is (list token, index); the playing song is kept as
/// a one-song list first. Doors marked "blocks" are called off the main thread.
/// Menu line codes `a`: 0 favorite (`on`), 1 play next, 2 add to queue, 3 add to playlist, 4 remove
/// download, 5 stop download, 6 download, 7 album (`id`), 8 artist (`id`, `name`, `named`), 9 add to
/// library, 10 sleep timer, 11 radio, 12 instant mix, 13 exclude from mixes, 14 share, 15 details.
int32_t nori_ios_keep_now(uint64_t token);
/// JSON to free: {items: [{a, more, on?, id?, name?, named?}], details}.
char *nori_ios_song_menu(uint64_t token, int32_t index, int32_t starred, int32_t player);
/// Radio and instant mix block and answer the songs played (-1 failed); otherwise 1 done.
int32_t nori_ios_song_act(uint64_t token, int32_t index, int32_t action, int32_t on);
/// The server's link to the song, to free; NULL when none. Blocks.
char *nori_ios_song_share(uint64_t token, int32_t index);
/// Playlist edits. Block; 1 when done.
int32_t nori_ios_playlist_add(uint64_t token, int32_t index, const char *playlist);
int32_t nori_ios_playlist_create(uint64_t token, int32_t index, const char *name);
int32_t nori_ios_playlist_remove(const char *playlist, int32_t index);
/// A tap on a song row: plays the list from it, or keeps the playing song going (1: open the player).
int32_t nori_ios_tap_song(uint64_t token, int32_t index);
/// A song row's swipe to the left (left 1) or right, by the swipeLeft/swipeRight settings: -1 nothing.
#define NORI_SWIPE_QUEUE 0
#define NORI_SWIPE_PLAY_NEXT 1
#define NORI_SWIPE_FAVORITE 2
#define NORI_SWIPE_UNFAVORITE 3
#define NORI_SWIPE_DOWNLOAD 4
int32_t nori_ios_row_swipe(int32_t left, int32_t starred);
int32_t nori_ios_playlist_delete(const char *playlist);
/// The sleep timer: its choices as JSON to free [{minutes, end, songs}]; songs or end of track (0, 0
/// cancel); a minutes timer's [delay ms, slack ms] as JSON to free; and the pause when it runs out.
char *nori_ios_sleep_choices(int32_t running);
void nori_ios_sleep_set(uint32_t songs, int32_t end_of_track);
char *nori_ios_sleep_delay(uint32_t minutes);
void nori_ios_sleep_now(void);

void nori_ios_remember(const char *query);
void nori_ios_sync(void);
/// A headed page's Play and Shuffle (album 8, artist 9, playlist 10, mix 15, smart 17) against the queue
/// now, packed as the core's HeroButtons::pack: bit 0 Shuffle lit, 2 pausing, bits 4-5 Shuffle's press
/// and 6-7 Play's (0 start, 1 toggle, 2 shuffle off). `playing` and `buffering` as the app shows them.
int32_t nori_ios_hero(int32_t kind, const char *id, int32_t playing, int32_t buffering);
/// The credits page's lists as JSON to free: {core, data, app}, each [{name, what, copyright, licence, file}].
char *nori_ios_credits(void);
/// Index and storage numbers as JSON (free it).
char *nori_ios_facts(void);

/// The active server's label (its name, otherwise the user, otherwise the address).
/// Empty when none are saved. NULL for an unreadable path. Free with `nori_ios_free`.
char *nori_ios_active(const char *data_dir);

/// Login result. `NORI_LOGIN_HTTP` and `NORI_LOGIN_OTHER` and `NORI_LOGIN_DATABASE` may set `detail`.
#define NORI_LOGIN_OK 0
#define NORI_LOGIN_INCOMPLETE 1
#define NORI_LOGIN_NOT_FOUND 2
#define NORI_LOGIN_UNREACHABLE 3
#define NORI_LOGIN_TIMEOUT 4
#define NORI_LOGIN_CERTIFICATE 5
#define NORI_LOGIN_HTTP 6
#define NORI_LOGIN_PASSWORD 7
#define NORI_LOGIN_FORBIDDEN 8
#define NORI_LOGIN_NOT_SUBSONIC 9
#define NORI_LOGIN_DATABASE 10
#define NORI_LOGIN_OTHER 11
#define NORI_LOGIN_CLEARTEXT 12
#define NORI_LOGIN_METERED 13

/// Pings the server and, when it answers, saves that profile as the active one. Does not open
/// playback. `detail`, when not NULL, receives a string to free with `nori_ios_free`, or is set NULL.
/// `form` is JSON {url, user, password, name, alt, key}: `alt` the second address, `key` an API key that
/// stands in for the password; the last three may be empty.
int32_t nori_ios_login(const char *data_dir, const char *form, char **detail);

#ifdef __cplusplus
}
#endif

#endif
