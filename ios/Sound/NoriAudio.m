// AURemoteIO for the iPod: playback session, float interleaved, the render callback is Rust
// (`nori_ios_render`). Nothing here decides: a route change, an interruption and a media-services
// reset are handed to Rust as they arrive.
//
// The unit takes the rate Rust asks for (the song's) and converts to the hardware's; Rust is granted
// exactly that rate, so what it writes and what the unit reads always agree. iOS moves the hardware rate
// after the session asks (the preferred rate takes effect late), and a unit built before such a move
// plays at the wrong speed or stops pulling, so the unit is built again, at the same client rate,
// whenever its hardware side changes. Every unit operation holds
// `lock`: the engine's thread, the notifications and the property listener all reach it.

#import <AVFoundation/AVFoundation.h>
#import <AudioToolbox/AudioToolbox.h>

#import "NoriAudio.h"

static AVAudioSession *session;
static AudioComponentInstance unit;
static id routeObserver;
static id interruptObserver;
static id resetObserver;
static uint32_t openChannels = 2;
static BOOL playing = NO;
/// The rate Rust writes at; the unit's input side.
static double clientRate = 0;
/// The unit's output side when it was built.
static double hardwareRate = 0;
static NSObject *lock;
static dispatch_queue_t rebuilds;

static void note(NSString *line) {
    nori_ios_audio_log(line.UTF8String);
}

static void put_err(char *err, uint32_t len, NSString *text) {
    if (!err || len == 0) {
        return;
    }
    NSString *s = text ?: @"the audio unit failed";
    [s getCString:err maxLength:len encoding:NSUTF8StringEncoding];
    err[len - 1] = 0;
}

static int32_t port_code(AVAudioSessionPort port) {
    if ([port isEqualToString:AVAudioSessionPortBuiltInSpeaker] || [port isEqualToString:AVAudioSessionPortBuiltInReceiver]) {
        return 1;
    }
    if ([port isEqualToString:AVAudioSessionPortHeadphones] || [port isEqualToString:AVAudioSessionPortHeadsetMic]) {
        return 2;
    }
    if ([port isEqualToString:AVAudioSessionPortBluetoothA2DP] || [port isEqualToString:AVAudioSessionPortBluetoothHFP] || [port isEqualToString:AVAudioSessionPortBluetoothLE]) {
        return 3;
    }
    return 0;
}

static void fill_grant(NoriGrant *out) {
    if (!out) {
        return;
    }
    memset(out, 0, sizeof(*out));
    AVAudioSessionPortDescription *p = session.currentRoute.outputs.firstObject;
    out->rate = (uint32_t)llround(session.sampleRate);
    out->io_ms = (uint32_t)llround(session.IOBufferDuration * 1000.0);
    out->latency_us = (uint64_t)llround(session.outputLatency * 1e6);
    out->port = p ? port_code(p.portType) : 0;
    NSString *name = p.portName ?: @"";
    [name getCString:out->name maxLength:sizeof(out->name) encoding:NSUTF8StringEncoding];
}

static void tell_route(BOOL lost) {
    NoriGrant g;
    fill_grant(&g);
    nori_ios_route(g.port, g.name, g.latency_us, lost ? 1 : 0);
}

static double hardware_rate(void) {
    AudioStreamBasicDescription hw;
    UInt32 size = sizeof(hw);
    if (!unit || AudioUnitGetProperty(unit, kAudioUnitProperty_StreamFormat, kAudioUnitScope_Output, 0, &hw, &size) != noErr) {
        return 0;
    }
    return hw.mSampleRate;
}

static void rebuild_if_moved(void);

static void format_changed(void *ref, AudioUnit u, AudioUnitPropertyID prop, AudioUnitScope scope, AudioUnitElement element) {
    (void)ref;
    (void)u;
    (void)prop;
    if (scope == kAudioUnitScope_Output && element == 0) {
        // Not from inside the unit's own notification: building it again disposes it.
        dispatch_async(rebuilds, ^{ rebuild_if_moved(); });
    }
}

static void dispose_unit(void) {
    if (!unit) {
        return;
    }
    AudioOutputUnitStop(unit);
    AudioUnitUninitialize(unit);
    AudioComponentInstanceDispose(unit);
    unit = NULL;
    playing = NO;
}

static void watch(void) {
    NSNotificationCenter *center = [NSNotificationCenter defaultCenter];
    if (routeObserver) {
        return;
    }
    routeObserver = [center addObserverForName:AVAudioSessionRouteChangeNotification object:session queue:nil usingBlock:^(NSNotification *n) {
        NSUInteger reason = [n.userInfo[AVAudioSessionRouteChangeReasonKey] unsignedIntegerValue];
        tell_route(reason == AVAudioSessionRouteChangeReasonOldDeviceUnavailable);
        dispatch_async(rebuilds, ^{ rebuild_if_moved(); });
    }];
    interruptObserver = [center addObserverForName:AVAudioSessionInterruptionNotification object:session queue:nil usingBlock:^(NSNotification *n) {
        NSUInteger type = [n.userInfo[AVAudioSessionInterruptionTypeKey] unsignedIntegerValue];
        BOOL began = type == AVAudioSessionInterruptionTypeBegan;
        BOOL resume = NO;
        if (!began) {
            NSUInteger opt = [n.userInfo[AVAudioSessionInterruptionOptionKey] unsignedIntegerValue];
            resume = (opt & AVAudioSessionInterruptionOptionShouldResume) != 0;
        }
        nori_ios_interruption(began ? 1 : 0, resume ? 1 : 0);
    }];
    resetObserver = [center addObserverForName:AVAudioSessionMediaServicesWereResetNotification object:session queue:nil usingBlock:^(NSNotification *n) {
        (void)n;
        @synchronized(lock) {
            dispose_unit();
        }
        nori_ios_media_reset();
    }];
}

static void unwatch(void) {
    NSNotificationCenter *center = [NSNotificationCenter defaultCenter];
    for (id obs in @[ routeObserver ?: [NSNull null], interruptObserver ?: [NSNull null], resetObserver ?: [NSNull null] ]) {
        if (obs != [NSNull null]) {
            [center removeObserver:obs];
        }
    }
    routeObserver = nil;
    interruptObserver = nil;
    resetObserver = nil;
}

static OSStatus render(void *ref, AudioUnitRenderActionFlags *flags, const AudioTimeStamp *ts, UInt32 bus, UInt32 frames, AudioBufferList *list) {
    (void)ref;
    (void)flags;
    (void)ts;
    (void)bus;
    if (!list || list->mNumberBuffers < 1 || !list->mBuffers[0].mData) {
        return noErr;
    }
    nori_ios_render(frames, (float *)list->mBuffers[0].mData);
    return noErr;
}

/// Builds the unit at `clientRate`. Holds `lock`.
static int make_unit(uint32_t channels, char *err, uint32_t err_len) {
    dispose_unit();
    AudioComponentDescription desc;
    memset(&desc, 0, sizeof(desc));
    desc.componentType = kAudioUnitType_Output;
    desc.componentSubType = kAudioUnitSubType_RemoteIO;
    desc.componentManufacturer = kAudioUnitManufacturer_Apple;
    AudioComponent comp = AudioComponentFindNext(NULL, &desc);
    if (!comp || AudioComponentInstanceNew(comp, &unit) != noErr) {
        put_err(err, err_len, @"no RemoteIO unit");
        return 1;
    }
    AudioStreamBasicDescription fmt;
    memset(&fmt, 0, sizeof(fmt));
    fmt.mSampleRate = clientRate;
    fmt.mFormatID = kAudioFormatLinearPCM;
    fmt.mFormatFlags = kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked;
    fmt.mChannelsPerFrame = channels;
    fmt.mFramesPerPacket = 1;
    fmt.mBitsPerChannel = 32;
    fmt.mBytesPerFrame = 4 * channels;
    fmt.mBytesPerPacket = fmt.mBytesPerFrame;
    OSStatus st = AudioUnitSetProperty(unit, kAudioUnitProperty_StreamFormat, kAudioUnitScope_Input, 0, &fmt, sizeof(fmt));
    AURenderCallbackStruct cb = {.inputProc = render, .inputProcRefCon = NULL};
    if (st == noErr) {
        st = AudioUnitSetProperty(unit, kAudioUnitProperty_SetRenderCallback, kAudioUnitScope_Input, 0, &cb, sizeof(cb));
    }
    if (st == noErr) {
        st = AudioUnitInitialize(unit);
    }
    if (st == noErr) {
        st = AudioUnitAddPropertyListener(unit, kAudioUnitProperty_StreamFormat, format_changed, NULL);
    }
    if (st != noErr) {
        dispose_unit();
        put_err(err, err_len, [NSString stringWithFormat:@"the audio unit would not open (%d)", (int)st]);
        return 1;
    }
    hardwareRate = hardware_rate();
    note([NSString stringWithFormat:@"audio unit built: %.0f Hz from Rust, the hardware at %.0f Hz (the session says %.0f)", clientRate, hardwareRate, session.sampleRate]);
    return 0;
}

/// The hardware side moved since the unit was built: built again at the same client rate, running
/// again if it was.
static void rebuild_if_moved(void) {
    @synchronized(lock) {
        double now = hardware_rate();
        if (!unit || now == 0 || now == hardwareRate) {
            return;
        }
        note([NSString stringWithFormat:@"the hardware moved from %.0f to %.0f Hz: the audio unit is built again", hardwareRate, now]);
        BOOL was = playing;
        char err[256];
        if (make_unit(openChannels, err, sizeof(err)) != 0) {
            note([NSString stringWithFormat:@"the audio unit would not build again: %s", err]);
            nori_ios_media_reset();
            return;
        }
        if (was && AudioOutputUnitStart(unit) == noErr) {
            playing = YES;
        }
    }
}

int nori_audio_open(uint32_t rate, uint32_t channels, uint32_t io_ms, NoriGrant *out, char *err, uint32_t err_len) {
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        lock = [NSObject new];
        rebuilds = dispatch_queue_create("nori.audio.rebuild", DISPATCH_QUEUE_SERIAL);
    });
    @synchronized(lock) {
    session = [AVAudioSession sharedInstance];
    NSError *e = nil;
    if (![session setCategory:AVAudioSessionCategoryPlayback error:&e]) {
        put_err(err, err_len, e.localizedDescription);
        return 1;
    }
    [session setPreferredSampleRate:(double)rate error:nil];
    [session setPreferredIOBufferDuration:(double)io_ms / 1000.0 error:nil];
    if (![session setActive:YES error:&e]) {
        put_err(err, err_len, e.localizedDescription);
        return 1;
    }
    openChannels = channels ? channels : 2;
    clientRate = rate ? (double)rate : session.sampleRate;
    if (make_unit(openChannels, err, err_len) != 0) {
        return 1;
    }
    watch();
    fill_grant(out);
    out->rate = (uint32_t)llround(clientRate);
    return 0;
    }
}

int nori_audio_start(char *err, uint32_t err_len) {
    @synchronized(lock) {
    if (!unit) {
        put_err(err, err_len, @"the output is not open");
        return 1;
    }
    NSError *e = nil;
    if (![session setActive:YES error:&e]) {
        put_err(err, err_len, e.localizedDescription);
        return 1;
    }
    OSStatus st = AudioOutputUnitStart(unit);
    if (st != noErr) {
        put_err(err, err_len, [NSString stringWithFormat:@"the output would not start (%d)", (int)st]);
        return 1;
    }
    playing = YES;
    return 0;
    }
}

void nori_audio_stop(void) {
    @synchronized(lock) {
        if (unit) {
            AudioOutputUnitStop(unit);
        }
        playing = NO;
    }
}

int nori_audio_set_io_ms(uint32_t io_ms, uint32_t *granted_ms, char *err, uint32_t err_len) {
    @synchronized(lock) {
    if (!session) {
        put_err(err, err_len, @"the output is not open");
        return 1;
    }
    NSError *e = nil;
    if (![session setPreferredIOBufferDuration:(double)io_ms / 1000.0 error:&e]) {
        put_err(err, err_len, e.localizedDescription);
        return 1;
    }
    BOOL was = playing;
    if (was) {
        AudioOutputUnitStop(unit);
    }
    // The new duration is taken when the unit starts again.
    if (make_unit(openChannels, err, err_len) != 0) {
        return 1;
    }
    if (was && AudioOutputUnitStart(unit) != noErr) {
        put_err(err, err_len, @"the output would not start");
        return 1;
    }
    playing = was;
    if (granted_ms) {
        *granted_ms = (uint32_t)llround(session.IOBufferDuration * 1000.0);
    }
    return 0;
    }
}

void nori_audio_route(NoriGrant *out) {
    fill_grant(out);
}

void nori_audio_close(void) {
    @synchronized(lock) {
        dispose_unit();
    }
    unwatch();
    NSError *e = nil;
    [session setActive:NO withOptions:AVAudioSessionSetActiveOptionNotifyOthersOnDeactivation error:&e];
}
