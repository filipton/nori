# AutoMix-style transitions for an Android player: research and design

Labels used: **[verified]** = stated by a cited source; **[inferred]** = my reasoning or estimate, not confirmed by a source.

The song analysis underneath (beats, bars, tempo, key, sections, vocals: methods, licences, what runs on a phone,
and how nori's analysis measures against a synthetic test set) has its own document, `analysis.md`. Where the
two disagree on the analysis, `analysis.md` is the later one.

## 1. What Apple's AutoMix does

**Apple's own wording.** Apple's WWDC25 services release says AutoMix uses AI "to analyze audio features" and "crafts unique transitions between songs with time stretching and beat matching" ([Apple Newsroom](https://www.apple.com/newsroom/2025/06/apple-services-deliver-powerful-features-and-intelligent-updates-to-users-this-fall/), [MusicTech](https://musictech.com/news/gear/apple-music-automix-ai/)). Press coverage adds that it looks at tempo and key ([MacRumors](https://www.macrumors.com/how-to/ios-enable-automix-feature-apple-music/)). No WWDC technical session covers the DSP.

**Limits and behaviour, from Apple Support** ([support.apple.com/105067](https://support.apple.com/en-us/105067)) **[verified]:**
- It only works with Apple Music subscription content. It does not work with uploaded or matched library files or iTunes purchases, even when the same song is in the catalogue.
- It is not available with hi-res lossless. On Mac it needs Apple silicon.
- It may not transition when an album plays in order, or when "the genres or tempos are incompatible". In those cases it falls back to a plain transition.
- Crossfade uses a fixed 1–12 s. AutoMix picks its own transition points and length.

**Server-side or on-device? [inferred]** The catalogue-only rule, with a clean copy of the same song being refused, strongly suggests the transition metadata is computed by Apple on the server for each catalogue track: beat grid, cue points and compatibility. The rendering (time-stretch, filters, mixing) happens on the device, which fits the Apple-silicon and no-hi-res requirements. Apple has not confirmed this. A third-party developer notes that MusicKit never exposes decoded protected audio to apps ([primuse #117](https://github.com/chenqi92/primuse/issues/117)), so a client could not analyse protected audio itself anyway.

**What it sounds like.** In iOS 26 the transitions were described as predictable, with a characteristic "underwater" sound ([9to5Mac](https://9to5mac.com/2026/07/16/ios-27-makes-one-of-my-favorite-apple-music-features-even-better/)). **[inferred]** That is almost certainly a low-pass filter sweep on the outgoing track, so the effect is DJ-style filtering and not only volume. In iOS 27 Apple reworked the intro and outro sections so the tempos line up, and it loops or repeats parts of intros and outros to bridge the two songs ([MacRumors](https://www.macrumors.com/2026/06/09/apple-music-gains-automix-upgrades-and-more-in-ios-27/), [RouteNote](https://routenote.com/radar/apple-music-automix-gets-a-major-upgrade-in-ios-27/)). Apple has not published how long transitions are or whether it uses EQ swaps.

**Complaints** ([How-To Geek](https://www.howtogeek.com/i-disabled-apple-music-automix/), [BGR](https://www.bgr.com/2059450/how-to-turn-off-automix-apple-music-worst-feature/)):
- The transition does not always happen.
- It sometimes cuts off final chords.
- It sometimes skipped "as much as a full minute" to line up the tempos.
- Volume spikes and "broken record" stutters.
- It made odd choices, such as jumping into the middle of the next song.
- Album listeners dislike it, and it suits house, techno and pop better than other genres.

**Lesson for us:** never throw away a lot of the song, keep the transition window bounded, and make fallbacks conservative.

### 1a. Follow-up (2026-09-29, second pass with open web access): what is actually known

**Short answer: still no primary source on the internals, and nothing [measured].** Apple's only technical sentence is the one in section 1. Nobody I could find has published a spectrogram, BPM trace or overlap length. What this pass settled is what the *sources say*, and which popular claims have no source. Quotes below come from pages read through a summarising fetch tool, so treat them as close paraphrase, not verbatim, unless marked otherwise.

**iOS 27 is the current version (released 2026-09-14, announced 2026-06-09), so read this block first.** Everything about iOS 27 below is from June 2026 or later.

- **What Apple changed [verified]:** "improved the underlying algorithms to generate new transition types, making for more seamless blends" ([MacRumors, 2026-06-09](https://www.macrumors.com/2026/06/09/apple-music-gains-automix-upgrades-and-more-in-ios-27/)); it still "blends songs using matching key and tempo", and Crossfade stays as an option. No transition type is named anywhere I could read.
- **What reviewers say changed [verified, opinion]:** iOS 26 "tends to use a very similar and predictable style for most transitions" and defaulted to an "underwater" effect that iOS 27 "largely replaced" with more natural blending; iOS 27 will "remix the outro and intro of songs to perfectly align the tempo" and "repeat parts of the outros and intros to bridge the transition" ([9to5Mac, 2026-07-16](https://9to5mac.com/2026/07/16/ios-27-makes-one-of-my-favorite-apple-music-features-even-better/), read through a summariser, so phrasing is approximate). RouteNote ([2026-07-17](https://routenote.com/blog/apple-music-automix-upgrade/)) says it feels "less like a programmed crossfade and more like a DJ smoothly mixing", but restates 9to5Mac and Apple; it is not independent testing. A post titled "Apple Music auto mix is way better on iOS 27" ([Max Weinbach on X](https://x.com/mweinbach/status/2068158525524570417)) exists, but the page returned 402 and only the title was seen, so it is **[unread]**.
- **So the "big improvement" is real as opinion, not as measurement.** The technique names (loop or repeat sections, re-cut intro and outro to align tempo) are the only new mechanics on record. Lengths, stretch limits, which song moves and what "new transition types" are: **[unknown]**.
- **Not everyone agrees it improved.** [Is iOS Stable on X](https://x.com/isiosstable/status/2079892915929649661) ("AutoMix Feedback on iOS 27.0", search snippet only, 402 on the page) compares Olivia Rodrigo "Good 4 U" into The Verve "Bittersweet Symphony": "The iOS 26 transition is a proper AutoMix - iOS 27 there is no mixing", with 10+ reports. A [June Apple Community thread](https://discussions.apple.com/thread/256311704) reports AutoMix and Crossfade "not functioning" on the beta and the release candidate. Both are user reports, not measured, and may be bugs or a stricter fallback. Nothing on 27.1 was found.
- **New platforms [verified]:** Apple TV 4K (2nd gen or later) with tvOS 27 ([MacRumors 2026-09-25](https://www.macrumors.com/2026/09/25/tvos-27-release-notes/): "transitions one song into the next like a DJ does") and HomePod, HomePod mini and the 2017 HomePod with HomePod software 27, described as "time stretching and beatmatching" ([MacRumors 2026-09-14](https://www.macrumors.com/2026/09/14/apple-releases-homepod-software-27/)). Because a HomePod does the mixing, it can not need a phone's Neural Engine, but that is my inference, not a statement.
- **Support page, dated 2026-09-14 as the fetch tool reported it** ([105067](https://support.apple.com/en-us/105067)): same fallback rules as before (album in order, incompatible genres or tempos, uploaded or purchased content, no hi-res lossless). Whether the wording changed since iOS 26 was not compared.
- **Still open for iOS 27:** any measured overlap, tempo limit or filter use; whether albums mix (no) and whether AirPlay and offline work (contested); a full read of the two X posts and Apple's iOS 27 newsroom page (a guessed URL returned 404). Searches found no iOS 27 hands-on from The Verge, Ars, Engadget, Tom's Guide, CNET, iDownloadBlog or Hacker News. A YouTube pass found no iOS 27 transcript; the only iOS 27 video seen is a 126-second ViWizard roundup ([link](https://www.youtube.com/watch?v=YjuuLL8AGpY)) with a "Better AutoMix" chapter and no detail, unread.

**What Reddit says about iOS 27 (16 threads, read in full from PDFs the owner printed on 2026-09-29; r/AppleMusic, r/iOSBeta, one r/Beatmatch).** This is the largest first-hand body of listening reports found, and it is the best evidence on what iOS 27 changed. All of it is user report: **[verified] that people say it, nothing [measured]**, and I could not hear the linked clips. Thread URLs are in `automix-next.md` §5. Quotes are exact and short.

- **The community is split, roughly evenly, on whether iOS 27 is better.** Better: "more subtle and natural" (top comment, thread "AutoMix iOS 27?"); "AutoMix was a 4 out of 10 for me. With ios27 I'm now at an 8"; "That 'underwater' sound you get sometimes is now replaced with smarter transition effects"; a house and techno listener finds it "better than your average wedding DJ"; "before out of 10 songs, maybe 3 had auto-mixes that made sense... 3 to 5". Worse: "they butchered it on 27"; "iOS27 has fixed the mixing into the middle of the next song but has all but destroyed any cool transitions... all it does now is a weird echo effect at the end of the song"; "Sounds very cheap compared to iOS 26"; several kept a playlist of iOS 26 transitions that no longer match ("mixes that worked on iOS 26 ... isn't the same on iOS 27. seems like they reworked the whole algorithm"). One user reports the whole 26 to 27 shift as "coverage expansion". A recurring third view: iOS 26 betas 2-5 were the best, then the release "neutered" it, and 27 restored variety ("It had really creative transitions ... Then they neutered it, 26RC and onward"; "now on 27RC I don't feel there's really any bad transitions anymore just amazing, good, or simple").
- **What is new in iOS 27, as heard:**
  - *Repeating a beat or a bar.* The first report is on iOS 27 dev beta 1: "this is automix repeating an actual beat in a song". Others: "it repeats a single beat over and over during a transition" ("Waste away, waste away, waste away"); "sometimes repeating the last measure sounds awesome. sometimes it can sound very jarring"; "similar to vocal chops/stutter edits"; "Cutting out most of the 1st drop to transition and loop the already playing one is cool, but I'm missing half the song ... the loop needs to be on the incoming track". Looping *vocals* is the loudest complaint: "They repeat lyrics or does echo out effect"; "it repeats the first phrase of the artist's rhyme several times ... a disrespect to the artists" (Portuguese, translated); "just looping a fragment of the song and ends it abruptly, no longer doing the fade and tempo adjustment". One user says a comparable loop already existed in iOS 26 betas when Repeat was on, "then later versions screwed it up".
  - *Landing on drops.* "It's really good at knowing when the drops are and timing the transitions to that"; "It seems like it does proper phrase-matching now, not just basic beatmatching" (a guess by a non-tester). The same trait annoys others: "I don't like when it transitions into a drop all the time, I feel like automix always rush songs".
  - *Gentler tempo change.* iOS 26: "massively altering the tempo of tracks from ages before the transition"; "It would slow or speed up tracks tremendously to sometimes be half or double the tempo of the next track". iOS 27 (one user): "It still changes the tempo of the track when it needs to but not in a way that you can tell it's coming from miles away."
  - *Better filter sweeps, less "underwater".* "its doing loops now too and better filter sweeps" (a Golden Gate beta user). Against it: one report of a stutter glitch in the last two bars "on top of the worse filtered out muffled transition" (iPhone 13), and "it is still lowering audio quality when the mix starts, its so noticeable when using airpods".
  - *Vocals edited.* One user says it "actually modifies the lyrics from the beginning song to the next song sometimes" (Fetty Wap "Again" into NBA YB "Murda Gang"), which reads like the vocal repeat above.
- **Timing, the only numbers anywhere:** one user of the release candidate: "started under 30 seconds or less during the current track and no more than 40 seconds into the next". Others disagree in both directions: "98% of the transitions are significantly shorter now" and "the transitions are a bit longer and a bit more on the money. Especially with the EDM stuff". The old complaint stands: "Sometimes it cuts a full minute into a song" ("it starts way too deep in a song", "cut off a whole minute and a half"; "still skips the first minute of the song, which makes it unusable"). Also "occasionally cuts off whole first verses" and "sometimes leaves long silences between songs instead of mixing them". A user asks for "an 'AutoMix but play as much of the song as possible' setting".
- **Song pairs named, with the user's verdict:** Nine Inch Noize "Hersey" into "Came Back Haunted" ("Crazy transition"); Sidney Charles "Canvas" into ARTMANN "Only Us"; I Monster "Return of the Blue Wrath" into Gorillaz "Stylo"; Andata & Ceres "Loca" into Argot "Club Bizarre (Edit)"; "This Is What You Came For" into "One Kiss"; "Guess ft billie" into Kim Petras "Treat Me Like a Sl*t"; Mojo "Lady Hear Me Tonight" into Stardust "Music Sounds Better With You"; a clip that samples Daft Punk "Technologic" (pair not named). Regressions on iOS 27: Eminem "Cinderella Man" into "Without Me" ("BPM change was so crisp. Now it's gone"); Von Dutch into Justice "Stress"; Oasis "Bring It On Down" (a user's mix "HATES the first 2 mins"). On iOS 26 versus 27, Olivia Rodrigo "Good 4 U" into The Verve "Bittersweet Symphony" (X post, earlier). "Biochemical (Extended Mix)" into "Breakaway (feat. Wilhelm) (Extended Mix)": tested on iOS 27 beta, "the transition was very long", while another user says EDM transitions were "very quick on iOS 26".
- **Rules and triggers, as users learned them:**
  - **Hi-Res Lossless disables AutoMix and Crossfade in iOS 27** (worked on iOS 26). Apple Feedback answered "Working as designed"; workaround is plain Lossless (up to 24-bit/48 kHz ALAC) for *all* the streaming and download tiers, and it still fails for some until Wi-Fi streaming is set to Lossless. This matches the Apple Support text. One user claims AutoMix "started forcing everything to play at 48KHz when enabled" and had trouble with mixed sample rates; unverified.
  - **Which songs:** Apple, community and personal playlists; "it only works with streaming songs, no imported songs" (one user's matched-library CD rips did not mix; another says it always worked for their library); not albums in order; "certain genres it doesn't even try to AutoMix (like opera and spoken word)"; alternative rock gets "a seamless Crossfade rather than a mix", hip-hop and EDM mix most; a user says setting the *genre tag* in Apple Music changes how well it works ("game changer") **[unverified]**, and a restaurant playlist sorted by key, BPM and energy "does such a good job".
  - **Offline is contested:** one says it "doesn't work offline, whereas it used to", another that it "requires an internet connection"; two say it works offline for them. A weak signal for server-side data, not proof **[unknown]**.
  - Also: "it works with AirPlay" (one reply, unverified); "out-of-sync automix when using CarPlay" (iOS 26 era); a bug showing AutoMix tips on every launch (Apple has "at least 10 similar requests" and a "potential fix"); the button greyed out until "indexing" finished on a new install.
- **Features people ask Apple for:** a thumbs up or down per mix; control over "how aggressive" it is ("I want it to sound like I'm in a club when I'm driving"); a per-playlist setting instead of a global one; an "every minute" mix for short drives; a "best for AutoMix" playlist sort by tempo, key and length (djay has one; the old PaceMaker app "would analyze songs and find choruses and drops and build ups", and let the user pick the transition point). Cider, a third-party Apple Music client, is said to have its own AutoMix.
- **How to record it for the §8.4 measurement:** threads show two people did it by AirPlaying the iPhone to a Mac and capturing with [Audio Hijack](https://rogueamoeba.com/audiohijack/) (the built-in recorder does not capture Apple Music). That is a workable capture path for the recording plan.

**The iOS 26 picture, for comparison** (these are the "before"; 2025 sources):

**Settled from Apple's own pages:**
- Mac guide ([support.apple.com/guide/music/muse5e9ec085/mac](https://support.apple.com/guide/music/muse5e9ec085/mac)) **[verified]:** works "with music from the Apple Music catalog" on iPhone, iPad, Apple-silicon Mac and Vision Pro (iOS/macOS 26 or later); it "automatically selects the best transition type" and may remove silence or "perform a simple crossfade" when a complex transition is not appropriate.
- Support 105067 ([link](https://support.apple.com/en-us/105067)) **[verified]:** no dynamic transition when an album plays in sequence, when "the genres or tempos are incompatible", for uploaded library content or iTunes purchases; not with hi-res lossless; not on Intel Macs, Watch, Android or Windows. It says nothing on where analysis runs.
- Apple Newsroom ([June 2025](https://www.apple.com/newsroom/2025/06/apple-services-deliver-powerful-features-and-intelligent-updates-to-users-this-fall/)) **[verified]:** "Using AI to analyze audio features, it crafts unique transitions between songs with time stretching and beat matching". No location given.
- 9to5Mac ([2025-06-16](https://9to5mac.com/2025/06/16/automix-apple-music-ios-26/)) **[verified]:** AutoMix "isn't an Apple Intelligence feature" and runs on iPhone 11 and later.
- iOS 27 ([MacRumors](https://www.macrumors.com/2026/06/09/apple-music-gains-automix-upgrades-and-more-in-ios-27/)) **[verified]:** "improved the underlying algorithms to generate new transition types"; also coming to Apple TV and HomePod.

**Where the analysis runs: [unknown].**
- "Entirely on device, on the Neural Engine" traces to AppleMagazine ([link](https://applemagazine.com/everyones-going-crazy-about-apple-automix-heres-why/)) and TechPulse ([link](https://techpulse.press/faq/ios-27-new-apple-music-features/)). Neither quotes Apple; the Apple pages they link do not contain it, and TechPulse cites a support article (HT213752) that I could not open. **No primary source exists; treat the claim as an unsourced embellishment.**
- The only opposite claim is a search snippet from mystats.music ("Likely, Apple has precomputed audio features for the catalog"), which is speculation.
- Patents: none found for Apple. Only prior art surfaced (US 7,518,053, Texas Instruments, beat-matched crossfade by playback speed, [Google Patents](https://patents.google.com/patent/US7518053B1/en); US 10,721,556, Spotify, per a search snippet). Justia and the USPTO mirror were blocked, so an Apple filing is **[unknown]**, not ruled out.
- No WWDC25 or WWDC26 session covers it; the WWDC26 MusicKit session ([link](https://developer.apple.com/videos/play/wwdc2026/254/)) does not mention transitions, and no MusicKit API for AutoMix was found.
- TechRadar's reviewer guesses "Perhaps some AI is being done on a server" and says the exact details "remain a mystery"; AppleMagazine hedges that Apple "Likely" precomputed beat and key data. Both are guesses **[unknown]**. An interview snippet attributed to Apple Music's Oliver Schusser ("AI helps with beat matching and making sure we find the right moment") is generic and its outlet was not found **[unverified]**.
- Apple ML research on this exact problem exists and is Apple-affiliated: [Downbeat Tracking with Tempo-Invariant CNNs](https://machinelearning.apple.com/research/downbeat-tracking-with-tempo) (Di Giorgi, Mauch, Levy; ISMIR 2020). It never mentions Apple Music or transitions, so any link to AutoMix is **[unknown]**, but it shows Apple has neural beat and downbeat tracking in-house.
- Apple patents found are older and not beat-aware: US 8,553,504 "Crossfading of audio signals" ([Google Patents](https://patents.google.com/patent/US8553504B2/en), filed 2008) analyses RMS and energy at the ends of each track, on the fly or from precomputed metadata, to choose when a fade starts (for example 15-20 s from the end) and a non-linear curve. That is Apple's own version of the MixRamp-style fallback in section 4, and it allows for precomputed per-track metadata. US 8,473,084 covers low-latency crossfade buffering only. A 2026 application, US 2026/0270530 A1 (per [Patentlyze](https://patentlyze.com/patent/apple-ai-generated-media-collections-smart-transitions/), unread), is about transition duration by media type, not beats. **No Apple patent on beat-matched song mixing was found; that is a weak negative** because Google Patents search would not render. Texas Instruments US 7,518,053, Spotify's US 10,721,556 and Echo Nest US 8,280,539 (3-5 s timbre-matched segues) are prior art, not Apple.
- The iPhone guide, read in a browser, adds: "Albums and some genres play without transitions." Public API: only the older MusicKit `Transition.crossfade(duration:)` from iOS 18 ([exploringmusickit.com](https://exploringmusickit.com/musickit-crossfade)); no AutoMix API and no WWDC25 or WWDC26 session on it.
- Ben Aqua says it "uses AI to analyze in real time" the outro and intro, and that it does not seem to work on songs uploaded from your own library. The first is only his claim; the second is consistent with catalogue-side data but does not prove it **[unknown]**.
- Only a measurement can settle it (does an offline-downloaded, airplane-mode catalogue track still mix?). That is a device task.

**Technique and behaviour, by evidence (table).** All rows are reviewer or user description; none is measured.

| Finding | Source | Label | iOS |
|---|---|---|---|
| Time-stretch and beat-match; ending of a fast song is slowed toward a slower next one, slow songs sped up before a faster one (so the outgoing side moves too, not only the incoming) | [Yahoo](https://tech.yahoo.com/audio/articles/tried-apple-musics-dj-feature-153000036.html) | [verified], reviewer's impression, "comically bad at times" | 26 |
| Tempo adjust described as acting on the incoming track (124 to 126 BPM) | [mystats.music](https://mystats.music/blog/apple-music-automix-2026) | weak: illustrative, SEO-style; conflicts with Yahoo | 26 |
| Sound of the default mix is "underwater" and predictable | [9to5Mac](https://9to5mac.com/2026/07/16/ios-27-makes-one-of-my-favorite-apple-music-features-even-better/) | effect [verified]; mechanism [unknown], though a DJ heard high-pass and low-pass filters (Ben Aqua, below), which fits a filter sweep | 26 |
| Outro and intro are remixed to align tempo, and parts repeated to bridge; "more natural" than 26 | 9to5Mac, [RouteNote](https://routenote.com/radar/apple-music-automix-gets-a-major-upgrade-in-ios-27/) | [verified], one reviewer; lengths [unknown] | 27 |
| Waits for the end of a vocal phrase before the next song kicks in; 95 % of examples sensible | [Cridland](https://james.cridland.net/blog/2025/apple-music-auto-mix-examples/) | [verified], author's judgement | 26 |
| Beat-matching a false ending gives a bad result (rated "0/10") | Cridland | [verified] | 26 |
| Long fade-outs or over ~10 s of trailing silence defeat it; electronic (trance, house) best, classic rock worst; a scrub can stutter up to three repeated beats | [AppleInsider](https://appleinsider.com/articles/25/06/10/apples-automix-in-macos-26-isnt-a-house-dj-but-is-a-good-fm-radio-simulator) | [verified], macOS 26 beta | 26 |
| Mix started far too early, "chopping the last 30 seconds off a song"; one clip cut a song at 25 s left and entered the next at 49 s, skipping its first verse | [Yahoo/TechRadar](https://tech.yahoo.com/audio/articles/apple-music-users-loving-automix-140000012.html) | [verified] as a user clip relayed by a journalist; not measured | 26 beta |
| Skipped "as much as a full minute" to align tempos; final chords cut | [How-To Geek](https://www.howtogeek.com/i-disabled-apple-music-automix/), [BGR](https://www.bgr.com/2059450/how-to-turn-off-automix-apple-music-worst-feature/) (ABBA, "Keep An Eye on Dan": fade-out overlapped the next song) | [verified] anecdote | 26 |
| No fixed duration; timing chosen from "key and tempo" | How-To Geek | [verified]; overlap length [unknown] | 26 |
| Fallbacks: silence trim, simple crossfade, brief gap; on some songs only one side mixes and the other "just skips" | Mac guide; [Apple Community](https://discussions.apple.com/thread/256143899) | [verified]; regression said fixed in 26.2 | 26 |
| No AirPlay speaker support | AppleInsider | [verified] at launch; a claim that 26.1 added it is from mystats only, weak | 26 |
| Uses "a high pass and a low pass filter" on the two songs, live-DJ style ("not just mixing the songs based on BPM", 1:23-1:45) | [Ben Aqua, YouTube, 2025-06-20](https://www.youtube.com/watch?v=7IbPywte4Ko), a self-described DJ, dev beta | [verified] as his description by ear; not measured. The only source naming filters | 26 beta |
| House into drum and bass: it "speed up the house song outro" then "slowly ramp up the BPM" (3:09-3:30), i.e. the outgoing song moves and the tempo ramps; pitch behaviour not stated | Ben Aqua | [verified] as his description; not measured | 26 beta |
| Mix sometimes starts with "40 or 30 seconds" of the song left; his example began about 18 s before the end and cut "almost 20 seconds" off the next intro | Ben Aqua | [verified], by ear | 26 beta |
| Works well for similar genre and BPM (house, drum and bass); death metal into drum and bass gives "weird transitions" that can shave a vital outro; rock, metal and classical without a clear beat "typically won't try to beat match"; without a beat it trims end silence and quiet intros like an "AI crossfade" | Ben Aqua | [verified] by ear | 26 beta |
| Does not seem to mix songs uploaded from the user's own library | Ben Aqua (agrees with Apple Support) | [verified] | 26 |
| Bass swap, echo or reverb, key-based pitch shift, hard cuts | none | **[unknown]**; nobody names them | - |
| Overlap length in seconds or bars; maximum stretch; ramp shape | none | **[unknown]** | - |
| Tempo gap or half/double handling | none beyond "tempos are incompatible" (Apple) | **[unknown]** | - |
| Named pair: Bon Iver "29 #Strafford APTS" into Mumford & Sons "The Wolf" fades out "just as the four-count intro" starts; sometimes a gap, sometimes an overlap; false endings "can definitely catch it out" | [TechRadar](https://www.techradar.com/audio/apple-music/automix-is-the-apple-music-feature-that-made-me-love-listening-to-music-on-my-iphone-again) (read in a browser) | [verified], reviewer listening | 26 |
| Users time it: fade out about 30 s before the end, next song fades in about 30 s in, both audible together for about 5 s | [Apple Community](https://discussions.apple.com/search?q=AutoMix) (search snippet, iOS 26.0.1) | user report, roughly [measured]; snippet only | 26 |
| Manual skips get no mix ("only work when you let it go to the end"); some pop songs skip the first minute with an audible tempo change; rock rarely mixes | MacRumors forums ([1](https://forums.macrumors.com/threads/automix.2458553/), [2](https://forums.macrumors.com/threads/apple-music-skipping-beginning-of-songs.2479279/)) | user reports | 26 |
| Over AirPlay the mix glitches; AirPlay support said to arrive in 26.1 | Apple Community; RouteNote (per a subagent, not re-read) | user report; weak | 26 |

**Strongest evidence, with the pairs and quotes we have.** Song pairs are almost absent: only ABBA "Keep An Eye on Dan" (BGR). Cridland's page likely names more (his false-ending "0/10" song's title did not survive the summary); re-read it in a browser if the pair matters. Short quotes (summariser-rendered, not guaranteed verbatim): "chopping the last 30 seconds off a song" (Yahoo/TechRadar); "wait until the end of the vocal phrase" (Cridland); "underwater" and "largely replaced by more natural transitions" (9to5Mac).

**Could not open:** reddit.com (blocked in both the fetch tool and the extension-driven browser; 16 threads were read instead from PDFs the owner printed, see above; the Threads clip and the twink.forsale clip linked in them, and all audio and video in those threads, were not heard); X posts (402, snippets only); Justia patents (403) and the USPTO PDF mirror; Google Patents search results (would not render); the MacRumors iOS 27 bug-fix forum thread (403); Apple's iOS 26 and iOS 27 newsroom pages and apple.com/ios/ios-27 (404 or blocked); Apple HT213752; podcastvideos.com and mystats.music (only partly); individual Apple Community threads beyond the ones cited (only search snippets). Read later in a real browser, so no longer blocked: the Apple iPhone guide, both TechRadar articles, Mac Observer and part of the MacRumors forums. Searches of Hacker News (one story, no comments), Gearspace, Digital DJ Tips, DJ TechTools, Mixed In Key and the Pioneer, djay and VirtualDJ forums turned up nothing technical on AutoMix, so their silence is a search result, not proof.

**What this changes for nori** (proposals only; the design sections are untouched):
- **Step 2, loops:** Apple's iOS 27 loop is now the most praised and most disliked change at once. Repeating a beat or bar under an incoming song is heard as "DJ-like", but a looped *vocal* is heard as "cheap" and "a disrespect". That backs the never-loop-vocals rule in §3 and instrumental-only bars; also loop the *outgoing* song's tail under the incoming, not by cutting into the incoming's first drop.
- **Step 3, tempo ramps:** iOS 26 stretched an outgoing song "tremendously", to half or double tempo, from far ahead; iOS 27 is described as still changing tempo but late and subtly. Keep our ±6-8 % cap and the ×½ and ×2 fold, glide the outgoing side, and start the glide close to the swap. Apple's actual limit is [unknown].
- **Step 4, natural blends:** "underwater" was dropped and users still hear "better filter sweeps". Keep the low-pass rise subtle and short, and avoid a muffled last-two-bars tail (one iOS 27 stutter report).
- **Skip cap and a setting (§3 plus §5 of `automix.md`):** the loudest iOS 27 complaint is still "cuts a full minute into a song". Our 15 s hard cap addresses it; add an "as much of the song as possible" option, and an aggressiveness control, both of which users ask Apple for.
- **Landing on the drop:** users praise that iOS 27 "knows when the drops are" and also say it "rushes songs". Our `drop_aligned` search is the same idea; keep the skip cap and drum-intro rule so it never rushes past a song's opening, and consider a per-pair "did this sound right" mark in the perf report (users ask for thumbs up and down).

**Open, needs a device:** overlap length, stretch range, bass or filter handling, and where analysis runs. Section 8.4's recording method is the only route; the public web has no ground truth.

## 2. Comparable features

- **Spotify Automix.** It looks at tempo, key, energy and rhythmic structure. Spotify, not the user, chooses the start and end points, and the user cannot change the overlap length ([Spotify Community](https://community.spotify.com/t5/FAQs/Automix-Overview/ta-p/5257278)). Its newer Mix feature for playlists shows waveform, key and BPM and offers transition presets such as "Fade" and "Rise", with EQ, effects and cue-point editing ([MusicRadar](https://www.musicradar.com/music-tech/spotify-responds-to-apple-musics-new-automix-feature-by-letting-you-turn-your-playlists-into-ready-made-dj-sets-with-seamless-transitions)). Spotify computed bars, beats, sections, key and loudness on its servers for years. Its Web API exposed this as `/audio-analysis` until access was cut on 27 Nov 2024 ([Music Ally](https://musically.com/2024/11/28/spotify-removes-features-from-web-api-citing-security-issues/)). This supports the idea that server-side analysis is standard in the industry.
- **Symfonium Smart Fades.** Experimental, and it "requires waveform extraction" ([Symfonium 12.3.0](https://symfonium.app/news/version-1230/)). The developer says that "smart fades with settings are crossfade" ([forum](https://support.symfonium.app/t/smart-fade-tuning/12114)). Users complain that songs with long tails still get faded, and that tracks with a loud start still get a fade-in ([feedback thread](https://support.symfonium.app/t/smart-fades-feedback-thread/7900)). **[inferred]** It seems to pick fade points from where the amplitude envelope crosses thresholds. It does not beat-match.
- **Plexamp Sweet Fades.** Based on MPD's MixRamp. The server measures EBU R128 loudness and works out how far two songs should overlap from the loudness ramps at the end of one and the start of the next ([Plex Labs](https://medium.com/plexlabs/plexamp-v3-9af3b10063b4)). In MPD, MixRamp tags store how loudness changes over time at each end of the song. Overlap is set where both songs sit at `mixrampdb` (e.g. −17 dB), and MPD can also analyse songs on the fly ([MPD docs](https://mpd.readthedocs.io/en/stable/user.html)). This is the cheapest "smart" transition and makes an ideal fallback.
- **Poweramp.** A fixed-length crossfade in milliseconds, with separate settings for automatic and manual track changes, plus fades on seek, play and pause ([guide](https://caninfotech.com/poweramp-music-player/poweramp-music-player-how-to-crossfade-between-two-tracks/)). No analysis.
- **djay Automix AI.** It "identifies rhythmic patterns and the best intro and outro sections", "calculates optimal fade durations and automatically applies parameter changes to EQs and filters". Neural Mix adds stem separation ([Algoriddim](https://help.algoriddim.com/user-manual/djay-pro-windows/mixing-basics/automix)). Rekordbox and Serato work from a beat grid computed offline, with cue points and phrase analysis (Rekordbox). **[not re-verified here]**
- **Open-source reference.** kumone PR #51 is a full AutoMix design built on vDSP ([kumone #51](https://github.com/missuo/kumone/pull/51)):
  - Analysis: spectral-flux onsets, BPM by autocorrelation with a log-normal prior, Ellis DP beat tracking, downbeat voting, phrase boundaries, RMS-based intro and outro landmarks, BS.1770 loudness, Krumhansl key, and how much vocals are present over time.
  - Five checks decide whether a pair may be mixed: loudness gap, timbre distance, tempo stability, key distance and vocal clash. Each pair then gets a transition type, from a short fade up to a beat-matched mix with EQ hand-over or a beat-synced echo-out.
  - It checks alignment at bar level with a 3 % tolerance, because beat-level checks failed: onset timing jitters by 5–13 %.
  - Offline rendering runs at 100–300× realtime.
  - It also falls back to plain whole-mix blending when rendering fails or is late.

  This is the closest public blueprint for what we want.

## 3. Algorithms

### Tempo and beat tracking
Standard pipeline, which suits mobile:
1. Downmix to mono and resample to about 22 kHz.
2. STFT (window 1024–2048, hop 512), then mel or log-magnitude bands.
3. Half-wave-rectified spectral flux gives the onset-strength envelope (about 43 frames/s).
4. Autocorrelate the envelope, or run a comb filterbank, over 60–200 BPM. Weight by a log-normal prior centred near 120 BPM.
5. Ellis (2007) dynamic programming finds the beat sequence. Each beat's score is its onset strength plus the best earlier score, minus a penalty for straying from the target beat period. Backtracking gives the path.

Ellis reported just under 60 % beat accuracy on MIREX-06 development data ([paper](https://www.ee.columbia.edu/~dpwe/pubs/Ellis07-beattrack.pdf)). Tempo is usually scored as Acc1 (within 4 % of the true tempo) and Acc2 (also counting ×2, ×3, ½ and ⅓ as correct). **Half and double tempo errors are the main failure mode** ([Hörschläger et al.](https://www.ifs.tuwien.ac.at/~knees/publications/hoerschlaeger_etal_smc_2015.pdf)).

For mixing, most octave errors do no harm. When comparing two tracks, compare BPM after folding ×½ and ×2, and beat-match at whichever level gives the smallest ratio.

Rust crates, both MIT/Apache:
- **`beat-track-rs`** is exactly Ellis 2007: mel spectral flux, autocorrelation with a log-normal prior, then DP. It uses rustfft and ndarray ([docs.rs](https://docs.rs/beat-track-rs)).
- **`stratum-dsp`** covers BPM, key and HMM beat grids, with optional ONNX ([docs.rs](https://docs.rs/stratum-dsp)).

aubio is GPL and Essentia is AGPL, so avoid both. Recommendation: implement it in our own Rust core (about 500 lines), using `beat-track-rs` as a reference or dependency. We already have FFT/DSP code.

**CPU [inferred estimate]:** a 4-min track at 22 kHz gives about 10k frames. rustfft on one Cortex-A7x core takes roughly 0.1 s. Onset detection, autocorrelation and DP add less than 50 ms. **Decoding dominates:** about 0.2–1 s per track for MP3, AAC or Opus, less for FLAC. Total: about 0.3–1.5 CPU-seconds per track, roughly 0.2–0.5 % of the track's duration.

### Downbeats and phrases (lightweight)
- **Downbeat phase:** test the 4 possible bar starts (assume 4/4). For each one, add up bass-band onset strength (kick) and chroma change (chords tend to change on the "1") at every 4th beat, then take the phase with the highest total. This is the "downbeat voting" approach.
- **Phrases:** use beat-synchronous features (RMS, bass energy, chroma). Compute a Foote novelty curve from the self-similarity matrix, or simply look for energy jumps. Keep only candidates that fall on multiples of 8 or 16 bars from the first downbeat.
- **Checking:** check the grid at bar level, not beat level (see kumone). Flag tracks with drifting tempo (live recordings, older music). The DP beat intervals show this through their variance, and such tracks should not be beat-matched.

### Intro and outro regions
- Compute the RMS and loudness envelope at about 10 Hz, per beat. Trim silence where the level stays below −50 to −60 dBFS.
- The outro candidate is the last phrase boundary before the energy drops, or the last 16–32 bars when the ending is steady. The intro candidate is the region before the first large energy or bass jump.
- Also store MixRamp-style points: when the end of the track falls below −17 dB relative to track loudness, and when the start rises above it. These are the fallback.
- **Vocal activity heuristic [inferred, rough]:** high energy in the 300 Hz–3.4 kHz band relative to the whole spectrum, together with the spectral flatness and centroid patterns vocals produce, smoothed per beat. It is enough to avoid overlapping two vocal sections, but not to detect lyrics. Stem separation is too heavy for a battery-first design.

### Time-stretching

| Library | Licence | Quality at ±2–8 % | Notes |
|---|---|---|---|
| **Signalsmith Stretch** | MIT | Very good; rated alongside Rubber Band R3 ([KVR](https://www.kvraudio.com/forum/viewtopic.php?t=623537)) | C++11, header-only. Rust crates `signalsmith-stretch` ([lib.rs](https://lib.rs/crates/signalsmith-stretch)) and `ssstretch`. It has a cheaper preset. Build it with optimisation on, because it is about 10× slower without ([docs](https://signalsmith-audio.co.uk/code/stretch/)). |
| **Bungee** | MPL-2.0 | Good (adaptive phase vocoder) | Supports Android. Rust bindings `bungee-rs` ([GitHub](https://github.com/bungee-audio-stretch/bungee)). |
| Rubber Band | GPL, or paid commercial licence | R3 is excellent, R2 is fine | R3 uses a lot of CPU ([licence](https://breakfastquay.com/rubberband/license.html)). |
| SoundTouch | LGPL-2.1 | OK for small changes (WSOLA), tuned for pop/rock | About 100 ms latency. `soundtouch` crate ([lib.rs](https://lib.rs/crates/soundtouch)). |
| media3 Sonic | Apache-2.0 | Poor for music | Based on PICOLA and aimed at speech; its author says music quality is "pretty poor" ([Sonic docs](https://github.com/waywardgeek/sonic/blob/master/doc/index.md)). Fine as a last resort for ≤2 %. |
| Resampling ("vinyl") | – | Pitch moves 0.34 semitone per 2 % | The cheapest option. Many DJs accept it at ±2 %. |

Recommendation: **Signalsmith Stretch** (MIT) inside the Rust core, linked statically through its crate. Use varispeed resampling for changes of 2 % or less when the user enables that mode.

**CPU [inferred]:** Signalsmith at the default preset on 44.1/48 kHz stereo probably uses a single-digit percentage of one big mobile core, and only during the 10–30 s window. Measure it on device.

### Key detection
Take chroma from the STFT, averaged over the track (better: weighted towards the intro and outro windows that will actually overlap). Correlate it with Krumhansl or Temperley profiles for all 24 keys. Expect about 70–85 % accuracy on tonal Western pop, with relative-key and fifth errors being common ([summary](https://github.com/Corentin-Lcs/music-key-finder)).

**Verdict:** do not reorder the queue by key, because users of a library player expect the queue to be respected. Use key distance as one input to the pair score: Camelot distance ≤1 allows a long harmonic overlap, a clashing pair gets a short overlap, drums only, or an echo-out. The cost is minimal because the chroma comes from the same STFT.

### Transition shaping
- **Equal-power curve** (cos/sin) for uncorrelated material. Linear or sine-squared for beat-matched, phase-locked content, where the two tracks add coherently.
- **Bass swap:** the incoming track starts with its lows cut (high-pass or low-shelf at about 150–200 Hz, −20 to −inf dB). On a downbeat at a phrase boundary, swap in one move, over about 1 beat: cut the outgoing lows and restore the incoming lows. Only one track ever carries the bass ([vibesdj](https://vibesdj.io/learn/techniques/eq-swapping), [Club Ready DJ School](https://www.clubreadydjschool.com/tribe-talk/getting-started/bass-swapping-dont-make-this-common-mistake)). Use a gradual swap when the incoming intro is sparse.
- **Filter sweep:** a low-pass on the outgoing track, from 20 kHz down to about 300 Hz over 4–8 bars (the "underwater" sound). Or a high-pass on the outgoing track as the incoming track comes in.
- **Echo-out:** feedback delay synced to the beat on the outgoing track, then cut. Use it for clashing pairs.
- All of this is biquads plus gains. Per-sample cost is negligible.

## 4. Recommended design (battery-first)

**When to analyse:**
1. **During normal playback, for free:** tap the PCM already passing through our audio chain. Decoding is already paid for, so feed a streaming analyser in the Rust core (STFT, onset envelope, chroma and RMS accumulators). At track end, run tempo, DP, downbeat, phrase and key, which takes tens of ms, and store the record.
2. **When a track finishes downloading or caching:** analyse it on a low-priority thread, or queue it.
3. **Backfill the library** with WorkManager, constrained to charging + unmetered + battery-not-low (+ device idle). The work is batched and can resume.
4. **Just in time (first play, no record):** about 20 s before the outro window, decode only the last ~45 s of the current track and the first ~45 s of the next. That is about 10 % of a full analysis. Grid confidence is lower, so use a more conservative transition.

**Storage:** one SQLite row per track, about 200–500 bytes. Key it on server id + file hash or duration, and store an `analysis_version`.

```
bpm REAL, bpm_confidence REAL, beat_offset_ms INT, tempo_stable BOOL,
downbeat_phase INT(0-3), first_downbeat_ms INT,
intro_end_ms INT, outro_start_ms INT,                -- phrase-aligned cues
cue_candidates BLOB  -- few (ms, bars, energy, vocal) tuples
lead_silence_ms INT, trail_silence_ms INT,
mixramp_start_ms INT, mixramp_end_ms INT,
loudness_lufs REAL, key INT(0-23), key_confidence REAL,
vocal_end_ms INT, vocal_start_ms INT
```

Beat times are not stored. The grid is rebuilt as `offset + n·60/bpm`. For tracks with drifting tempo, beat matching is simply turned off.

**Planning at playback (Kotlin):**
1. Take the records for the current track A and next track B, and compute the tempo ratio after folding ×½ and ×2.
2. Beat-match only if both grids are confident and stable and the ratio is within the user's max (default ±6 %).
3. Choose A's outro cue and B's intro cue on phrase boundaries, avoiding vocal-on-vocal overlap. Never skip more than about 15 s of either track. This directly addresses Apple's "skipped a minute" complaint.

**Rendering (Rust):**
- Run both decks through the existing chain. Only B is stretched during the window, locked to A's tempo.
- After the swap, ramp B back to its native tempo over 4–8 bars, then bypass the stretcher.
- Apply the filters and bass swap, with loudness matching from LUFS or ReplayGain.
- **Outside the window the extra cost is zero.** During it, one stretcher plus about 6 biquads run for 10–30 s.

**Navidrome:** OpenSubsonic `Child` exposes `bpm` and `replayGain` ([OpenSubsonic Child](https://opensubsonic.netlify.app/docs/responses/child/), [Navidrome PR #2597](https://github.com/navidrome/navidrome/pull/2597)). Use `bpm` as a prior to settle half/double tempo, and as a prefilter to skip pairs that cannot be matched without decoding anything. It has no beat phase, so it cannot drive beat matching alone. Tag quality varies.

**Fallback ladder:**
1. Full analysis: beat-matched mix with bass swap.
2. Grid missing or unreliable: phrase-less crossfade at MixRamp or silence-trimmed points, with a filter sweep.
3. Nothing is known: a fixed equal-power crossfade.
4. Same album, played in order (or gapless-tagged): gapless, with no mixing.

## 5. Settings

- AutoMix on/off (separate from Crossfade).
- Style: Smart fade only / DJ mix.
- Transition length: auto, or a maximum in bars/seconds (e.g. 4–32 bars).
- Beat matching on/off.
- Allow tempo change on/off, with a maximum change of 2/4/6/8 %.
- Keep pitch (time-stretch) vs varispeed.
- Bass swap on/off. Filter effects on/off.
- Skip transitions within albums (default on), and respect gapless.
- Also transition on manual skip.
- Loudness matching.
- Analyse library only on charger + Wi-Fi (default on).
- Per-track exclusion ("never mix this track").

## 6. Competitive audit (2026) and what we ship

| Capability | Apple Music AutoMix | DJ.Studio Harmonize | Symfonium / Plexamp | **nori** |
|---|---|---|---|---|
| Beat match + time-stretch | yes (catalogue AI) | yes (offline edit) | no / MixRamp only | **yes, on-device** |
| Bass swap | not documented | yes | no | **yes** |
| LPF / filter sweep | yes (iOS 26 “underwater”; iOS 27 softer) | yes + HPF presets | no | **yes; Camelot-softened** |
| Echo-out for clashes | simple fade fallback | yes | no | **yes** |
| Camelot-aware length | inferred (key+tempo) | yes (bars 4–32) | no | **yes (≤1 long, 2 short, ≥4 echo)** |
| Loudness match | yes (catalogue) | yes | MixRamp / RG | **LUFS when RG off** |
| Max skip bound | criticised (≤1 min) | n/a (edit) | n/a | **15 s of music, hard cap; silence free** |
| Enter the next song on its drop | not documented | yes (manual cues) | no | **yes** (section 7) |
| Leave before a dead ending or hidden track | criticised for false endings | manual | no | **yes, within the cap** |
| Two singers kept apart | unknown | EQ lanes | no | **vocal duck + high-pass ride** |
| Phrase-matched start and swap | phrase-aligned (inferred) | yes | no | **yes** |
| Album-in-order gapless | yes | n/a | yes | **yes** |
| Works on self-hosted library | **no** (catalogue only) | yes (files) | yes | **yes** |
| Hi-res / USB DAC path | blocked on hi-res | n/a | varies | **offload-aware** |
| Intro/outro loop remix | **iOS 27** | loop effects | no | **outro loop remix** (intro live-loop deferred) |
| Reorder playlist by key | no (queue respected) | **yes (Harmonize)** | no | **no** (by design: library player) |
| Stem separation | no | optional | no | **no** (battery) |
| Tag BPM half/double prior | inferred | yes | n/a | **yes** |
| DJ filter-open (HPF) | soft in iOS 27 | yes | no | **yes** (Camelot stretch pairs) |

**Verdict.** For a library player that respects queue order, we match or beat Apple on self-hosted music: on-device analysis, bass swap, clash echo-out, MixRamp fallback, hard skip cap, Camelot-scaled length/filters, LUFS match, tag-BPM octave correction, and outro loop remix when the ending is too short for the target overlap. Still behind Apple’s catalogue intro looping (needs a second decode source) and DJ.Studio’s playlist reordering — deliberate non-goals for a queue-respecting library client.

## 7. Entering on the drop, leaving before a dead ending, two singers (2026-09)

The owner compared AutoMix with BitChord's and asked for three things: enter the next song on its drop, let the
outgoing song's exit be an interior point when its ending is not worth playing, and keep two singers apart with
filters rather than rerouting to an echo-out. Beside them, this section surveys how the other automatic mixers and
DJ practice handle transitions, and says which of those ideas were built.

Hosts other than GitHub could not be opened from here (Spotify, Mixxx's site, Algoriddim, DJ.Studio, most blogs).
Where a page could not be read, the fact below comes from the search engine's excerpt of it and is marked
**[excerpt]**; **[read]** means the page or file itself was read. BitChord (GPL-3.0) and Orchard (AGPL-3.0 from
4.0; releases up to 3.x were MIT) were read for facts only; no code was taken.

### 7.1 What others do

- **Apple Music AutoMix.** iOS 27 remixes intros and outros so tempos align, and repeats parts of them to bridge
  two songs ([MacRumors](https://www.macrumors.com/2026/06/09/apple-music-gains-automix-upgrades-and-more-in-ios-27/),
  [RouteNote](https://routenote.com/blog/apple-music-automix-upgrade/)) **[excerpt]**. The complaints are about
  where it cuts: starting the mix early enough to chop the last 30 s of a song, starting the next one 49 s in (a
  Taylor Swift example that skipped a first verse), and being caught out by false endings
  ([TechRadar](https://www.techradar.com/audio/apple-music/apple-music-fans-are-obsessed-with-automix-in-ios-26-but-one-big-flaw-could-be-its-downfall))
  **[excerpt]**.
- **Spotify Automix and Mix.** Automix trims intros and outros and aligns tempo within limits; Mix (beta, 2025)
  shows waveform, BPM and key and offers presets: **Fade** (a crossfade with the bass swapped around the midpoint),
  **Rise** (an overlap with the bass swap at the end, low-pass in and high-pass out) and **Blend** (a smooth
  three-band EQ fade), each editable as volume, EQ and effect curves
  ([Spotify](https://newsroom.spotify.com/2025-08-19/mix-your-favorite-playlists-seamlessly-by-adding-your-own-transitions/),
  [Yahoo Tech](https://tech.yahoo.com/audio/articles/spotifys-mixing-feature-lets-dj-093000572.html)) **[excerpt]**.
- **djay Automix AI.** Finds "the best intro and outro sections" and rhythmic patterns, automates EQs and filters,
  and with Neural Mix splits the songs into stems during a transition (a reverb on the outgoing vocal, say);
  transition types include Dissolve, Riser and Echo
  ([MusicTech](https://musictech.com/news/gear/algoriddim-free-dj-software-djay-pro-ai-automix-and-neural-mix/),
  [DJ Mag](https://djmag.com/news/new-djay-ai-ios-adds-improved-ai-mixing)) **[excerpt]**.
- **Mixxx Auto DJ.** Uses intro and outro cues (set by silence detection, editable). *Full Intro + Outro*, the
  default, starts the next track during the outro so that **the end of the intro lines up with the end of the
  outro**; *Fade At Outro Start* lines up their starts and cuts the rest of a longer outro; the crossfade is the
  shorter of the two sections
  ([Mixxx manual source](https://github.com/mixxxdj/manual/blob/2.4/source/chapters/djing_with_mixxx.rst),
  [wiki](https://github.com/mixxxdj/mixxx/wiki/Auto%20DJ%20Cues)) **[read]**.
- **rekordbox.** Phrase analysis labels Intro, Up, Down, Chorus, Verse, Bridge and Outro according to a track's
  "mood"; its Automix uses beat position, BPM and key
  ([Phrase Edit guide](https://cdn.rekordbox.com/files/20200312172204/rekordbox5.1.0_Phrase_Edit_operation_guide_EN.pdf))
  **[excerpt]**. Serato's Autoplay plays tracks back to back with no crossfade
  ([Serato](https://support.serato.com/hc/en-us/articles/202304934-Can-Serato-DJ-auto-mix-my-songs)) and Engine DJ
  users are still asking for an auto-mix
  ([Engine DJ community](https://community.enginedj.com/t/auto-mix-needed-for-engine-dj-stand-alone-controllers/55157))
  **[excerpt]**.
- **DJ.Studio.** Transition presets are volume, EQ and effect curves (slow crossfades, mid-band blends, filter
  sweeps, instant bass swaps); lengths are set in bars; Harmonize uses the Camelot wheel and lets the user choose
  how tempo is carried across ([help](https://help.dj.studio/en/articles/7878402-harmonize-previously-automix))
  **[excerpt]**.
- **Mixed In Key.** Scores energy 1 to 10 from the content (hi-hat patterns, noise risers), not the tempo, and
  advises mixing within one level for a steady set
  ([Mixed In Key](https://mixedinkey.com/harmonic-mixing-guide/sorting-playlists-by-energy-level/)) **[excerpt]**.
- **Plexamp Sweet Fades.** MPD's MixRamp on EBU R128 loudness (section 2)
  ([music-assistant discussion](https://github.com/orgs/music-assistant/discussions/3929)) **[read]**.
- **Orchard.** "Beat-matched, phrase-aligned AutoMix transitions with 3-phase volume curves, progressive filter
  sweeps, downbeat quantization, and bass swaps", on-device beat analysis on mobile, BPM from GetSongBPM
  ([README](https://github.com/SFG5453/Orchard)) **[read]**.
- **BitChord** (read-only clone, `playback/smart/*`, `native/analyzer/*`) **[read]**. Entry candidates are scored:
  a "main drop" (weight 0.5), an "intro drop" (0.4), the audible start (0.15) and phrase lines (0.1), plus 0.1 on a
  downbeat, minus 0.2 for a cold open (under four beats of run-up) and plus up to 0.2 for an instrumental run-up
  over the 16 beats before. Its "main drop" is simply 32 beats after the first downbeat when that is inside the
  first 40 % of the song, its intro drop the first 8-bar line capped at 36 s. Exits are an "energy cliff" (a late
  silence, backtracked to where the level fell), the outro start or the end of the content, under a 12 s budget of
  skipped music in which silence (below a tenth of the loud level) is free. Two voices are handled by filter rides
  scaled by how much they overlap: the outgoing song low-passed towards 1.6 kHz, the incoming one high-passed from
  700 Hz (520 to 950 Hz in a beat-matched blend) and opened by 45 to 70 % of the mix.
- **DJ practice.** A phrase is eight bars (32 beats) and sections change on phrase lines; the incoming track starts
  on beat one of a phrase and its intro is laid over the outgoing outro so that both turn together
  ([Native Instruments](https://blog.native-instruments.com/phrase-mixing/),
  [Wikipedia](https://en.wikipedia.org/wiki/Phrasing_(DJ))) **[excerpt]**. Voices sit between about 200 Hz and
  4 kHz; to stop two clashing, cut the incoming track's mids during the blend and swap them over as the tracks
  change hands ([Digital DJ Pool](https://digitaldjpool.com/blog/dj-eq-mixing-for-beginners/),
  [Home DJ Studio](https://homedjstudio.com/dj-eqing/)) **[excerpt]**. Key clashes matter only where melodic parts
  overlap; percussive intros and outros are key-neutral
  ([Pioneer DJ](https://blog.pioneerdj.com/djtips/how-do-djs-approach-harmonic-mixing/),
  [Digital DJ Pool](https://digitaldjpool.com/blog/harmonic-mixing-camelot-wheel/),
  [OpenKeyScan](https://www.openkeyscan.com/harmonic-mixing-for-house-music)) **[excerpt]**. And for energy: do not
  mix from a high-intensity section into an intro, which drops the floor
  ([DJ.Studio](https://dj.studio/blog/anatomy-great-dj-mix-structure-energy-flow-transition-logic)) **[excerpt]**.

### 7.2 What was built

All of it is in nori-player's `automix` (`plan.rs`, `structure.rs`, `loudness.rs`, `mixer.rs`), which every
platform shares; nothing outside it changed but the stored rows' columns. `ANALYSIS_VERSION` is 8, so rows are
measured again as songs play.

1. **Enter on the drop.** The analysis finds where the arrangement arrives (`drop_point`): the first four-bar line
   in the opening (40 % of the song, 75 s at most) where the four bars after it reach the body of the song - within
   2 to 3 dB of its median bar in level, low end and chord energy - and the bars before lacked one of them by 3 dB.
   A drum intro has the level but not the chords, a pad the chords but not the low end, so neither is the drop; the
   full band arriving is. It stores the voice share of the eight bars before and after. The planner then searches
   (`drop_aligned`): every landing of the incoming song (the drop, the end of its intro, its four-bar lines, its
   first downbeat) against every downbeat of the outgoing song's last sixteen bars, with run-ups of sixteen bars
   down to none and a tail of a beat to four bars after the swap. The swap is where the landing meets the downbeat,
   so the drop, the bass swap (now over the sixteenth before the line, so the drop's first kick has all its low
   end) and both songs' section change coincide - Mixxx's *Full Intro + Outro*, Spotify's *Rise*. Windows that
   skip more than 15 s of music of either song are never considered; the rest are scored on the landing (drop 4,
   intro end 3, phrase line 1, straight in 0), skipped music (0.06 per second of an instrumental end, 0.15 of a
   sung one), the incoming intro left on its own after the mix (0.1 per second: the energy hole), the run-up laid
   under the outgoing song, a phrase line of the outgoing song, and a sung run-up under a sung ending. When the
   outgoing song has too few bars for the run-up and they are not sung, its last four or eight are read round (the
   iOS 27 outro remix, now a candidate in the search rather than a separate path; a sung loop is Apple's "broken
   record"). *Limits:* a drop further in than 15 s plus the longest run-up the mix length allows is out of reach (a
   16-bar drum intro under a 16 s mix); the drop is only as good as the grid (none without one, one bar off on a
   half-time grid).
2. **Leave before a dead ending.** The analysis finds the last silence of 6 s or more inside the music
   (`last_gap`) and a closing breakdown (`breakdown`): the earliest bar line in the last 24 s where the level falls
   6 dB below the four bars before, the beat goes with it (6 dB of low end or a third of the onsets) and no bar
   comes back within 3 dB - a pad coda, a breakdown the song never returns from, a fade-out. The exit is that
   breakdown, or the gap when the music after it is 15 s or less (a short hidden track). The planner's `Ending`
   charges only music against the cap - the gap and the silence at either end of a file are free - and aims the
   swap at the exit, so the incoming drop lands where the outgoing energy leaves, the breakdown falls away under
   it for up to four bars and the rest (within the cap) is not played. The same rule applies to echo-outs,
   one-grid fades and MixRamp fades. *Limits:* a hidden track longer than 15 s is music, so it is played, and the
   silence before it with it; a false ending with more than 15 s of song after it is not an exit; the sink drops
   the skipped remainder by decoding through it (see `docs/handoff.md`).
3. **Two singers.** The mixer has a vocal duck on the incoming deck: a band-pass around 1 kHz (Q 0.35, 3 dB down
   near 300 Hz and 3.3 kHz) subtracted in proportion, which is a peaking cut whose depth can move sample by sample
   without touching the filter's state, so it releases without a click and leaves the deck untouched, bit for bit,
   when off. The plan holds the incoming voice band 18 dB down at 1 kHz (about 6 dB at 500 Hz and 2 kHz) while
   the outgoing song still leads, releasing it over the beat before the swap, and after the swap rides a high-pass
   on the outgoing deck from 200 Hz to 2 kHz over the first half of what is left, so its voice thins to breath
   while it falls away - the DJ's mid swap, with the song taking over always the clear one. Each half only where
   both sing then. A pair whose voices overlap now gets this beat-matched mix; it echoes out only when no mix fits
   or the filters are off. A plain fade holds the incoming voice down through its first half. One band-pass per
   channel on the incoming deck while the duck is on: the mixer with every effect on costs 2.3 ms of CPU per second
   of audio (0.23 % of one core) on the desktop measured. *Limit:* it triggers on what the analysis calls sung, and
   the voice-band share cannot tell the synthetic singing (0.28 to 0.37) from an unsung band (0.20 to 0.34) or pads
   (0.8) - `analysis.md`'s open problem. The harness therefore also runs with the voices taken from the truth.

Chosen from the survey:

4. **A drum intro is laid under any key.** Key clashes need two melodic parts; the analysis stores the chord energy
   of the run-up to the drop (its most chordal four bars, a beatless opening included), and a run-up at least 3 dB
   below the song's chords is exempt from the Camelot and timbre caps on length, which then hold only after the
   swap, where both songs are whole.
5. **Phrase-matched starts.** Run-ups of whole four-bar phrases are preferred, so the mix starts on a phrase line
   of both songs as well as swapping on one.
6. **Loudness through the mix, measured.** Apple is criticised for volume spikes; the harness now renders each
   transition through the real mixer and compares its loudest 3 s with either song's own. It stays within +0.8 LU
   (+1.5 LU with 32 s mixes), so the gain law was left as it is.

Left out: reordering the queue by key or energy (a library player keeps the queue; DJ.Studio's Harmonize does
this), stem separation (djay's Neural Mix; battery and model licences, `analysis.md`), a riser (Spotify's Rise
low-passes the incoming song into the drop; a matter of taste the harness cannot score, and the drop landing on
the swap already makes the arrival the event), an intro loop (needs a second decoder on the incoming song), a
separate energy score in Mixed In Key's manner (with a fixed order it could only change the transition, and the
drop landing already avoids mixing a full song into a bare intro), and keeping a vocal pickup before the incoming
song's first downbeat (the synthetic songs have none to test it on).

### 7.3 How it was measured

`eval.rs` gained a transition harness. Fifteen pairs of synthetic songs (twelve new songs: drum intros longer than
a mix, a pad-drums-drop build, codas, a sung coda over keys, hidden tracks after 37 s of silence with 4 and 16 bars
of music after it, silence at the ends of the files, singing to the very end and from the first bar; plus pairs
from the analysis corpus) are rendered, analysed as the app would, planned with the app's settings and scored
against the truth: where the incoming drop lands against the swap, the swap on the outgoing song's bars and phrase
lines, music skipped, silence heard, the run-up laid over a dead ending, two voices competing (both sounding and
neither 10 dB under the other across 500 Hz to 2 kHz, measured by running the real mixer with tones on one deck at
a time), and loudness. `landmark_eval` scores the drop and exit detection on all 29 songs.

```sh
cargo test --release -p nori-player transition_eval -- --ignored --nocapture
cargo test --release -p nori-player landmark_eval -- --ignored --nocapture
NORI_EVAL_VERBOSE=1 NORI_EVAL_ONLY=hidden,coda cargo test ...   # the plan's reason per pair; a subset
```

Before is the planner and mixer as they were, scored by the same harness **[measured]**:

| Default settings (16 s at most), 15 pairs | Before | After |
|---|---|---|
| Incoming drops the swap lands on | 4 of 14 | 8 (10 with 32 s mixes, against 5) |
| Incoming intro left on its own after the mix | 90.2 s | 54.6 s (32 s mixes: 88.7 to 42.0) |
| Swaps on a downbeat / a phrase line of the outgoing song | 10 / 4 of 11 | 12 / 12 of 12 |
| Mixes starting on a phrase line | 4 of 11 | 10 of 12 (12 with 32 s mixes) |
| Music skipped, outgoing / incoming | 48.7 / 1.8 s | 58.8 / 39.9 s (none over the cap) |
| Silence heard before the mix | 58.5 s | 37.5 s (all of it the hidden track too long to leave) |
| Run-up laid over a dead ending (a coda, a gap, silence) | 18.4 s | 0 |
| Voices competing, the analysis's own vocal gate | 14.5 of 23.1 s sung together | 12.1 of 21.1 s |
| Voices competing, voices from the truth | 2.1 of 3.6 s (three echo-outs) | 5.3 of 21.1 s (12.7 without the separation; one echo-out) |
| Louder than either song | +0.3 LU mean, +1.1 at most | +0.4, +0.8 |

Landmarks over the 29 songs: drops 14 of 16 found within a beat, one false alarm (the one-drop, read at double
tempo); exits 3 of 3, no false alarms. The analysis corpus's own numbers (section 3 of `analysis.md`) are
unchanged, and the analysis costs 57.9 ms per minute of audio against 57.3.

What the numbers do not show, and what must be listened to on a phone: whether landing the drop on the swap and
letting the outgoing song go a beat after it sounds like a DJ or like a jump on real records; whether the 18 dB
vocal duck and the high-pass ride sound like two singers handing over or like a filter; whether skipping up to
15 s of an instrumental intro is noticed; whether leaving on a closing breakdown or before a short hidden track is
welcome or feels like a song cut short.

## 8. Research brief: how Apple's AutoMix really sounds (for an agent with open web access)

**Status:** done on 2026-09-29; the findings are in section 1a. What remains open needs a recording (8.4).

Written 2026-09-29 by a session whose network allowlist blocked almost every publisher, so section 1 and 1a rest on search snippets. This section lets another agent, with unrestricted fetching, close the gap without rereading the whole repo. Read section 1, 1a, and `automix-next.md` §1 first (about 5 minutes); don't redo what they hold.

### 8.1 The question

What does Apple Music's AutoMix (iOS 26, reworked in iOS 27) actually do to the audio, according to people who listened closely? Concretely, for each item the answer is one of **[verified]** (a named source says it), **[measured]** (someone published numbers or a spectrogram), **[inferred]**, or **[unknown]**:

1. Technique per transition: plain crossfade, low-pass or high-pass sweep, EQ or bass swap, echo or reverb tail, loop or repeat of intro/outro, tempo ramp, or hard cut.
2. Length of the overlap, in seconds and in bars, and how it varies between pairs.
3. How tempo is matched: fixed stretch ratio, gradual ramp, which track moves, largest change seen, and what happens when tempos are far apart or half/double.
4. Where in each song the mix starts and ends (phrase boundaries, downbeats, lulls, on the drop, in the outro's last seconds), and how much of a song gets skipped.
5. What triggers the fallback (plain crossfade, silence trim, no mix): album order, genre, tempo gap, key clash.
6. Where the analysis runs: on device (Neural Engine?) or precomputed by Apple for catalogue tracks. Find a primary source, or say **[unknown]**.
7. Known failure modes, with concrete song pairs where possible.
8. iOS 26 versus iOS 27 differences, described by someone who heard both.

### 8.2 What is already settled, so don't re-fetch it

Apple's wording (time stretching, beat matching, tempo and key); catalogue-only and no hi-res lossless; the Apple-silicon Mac requirement; Crossfade's 1-12 s; the iOS 27 claims of re-cut intros and outros, repeated sections, and a more natural, less "underwater" sound; the complaints in section 1. These come from Apple Support (105067), Apple Newsroom, MacRumors, 9to5Mac, RouteNote, MusicTech and How-To Geek. Confirm quotes only if you touch them.

### 8.3 Sources to fetch, in priority order

The first three groups are where the specific answers most likely are. Everything below was blocked in the last session, so none of it has been read in full.

**Hands-on listening write-ups** (extract: named song pairs, described technique, length, faults):
- https://james.cridland.net/blog/2025/apple-music-auto-mix-examples/ (examples, and possibly audio)
- https://appleinsider.com/articles/25/06/10/apples-automix-in-macos-26-isnt-a-house-dj-but-is-a-good-fm-radio-simulator
- https://tech.yahoo.com/audio/articles/tried-apple-musics-dj-feature-153000036.html
- https://www.techradar.com/audio/apple-music/automix-is-the-apple-music-feature-that-made-me-love-listening-to-music-on-my-iphone-again
- https://www.howtogeek.com/i-disabled-apple-music-automix/
- https://mystats.music/blog/apple-music-automix-2026
- https://digdis.de/en/blog/post/apple-music-automix-smooth-dj-transitions-directly-in-the-app

**DJ and producer communities** (look for people who name techniques: "bass swap", "filter", "phrase", "bars", "loop", "echo"):
- https://community.enginedj.com/t/apple-music-to-add-automix-in-ios-26/64354
- Reddit: r/DJs, r/Beatmatch, r/AppleMusic, r/audioengineering, r/musicproduction, r/iOSBeta. Search "AutoMix", "Apple Music AutoMix DJ", "AutoMix iOS 27".
- Gearspace, Digital DJ Tips, DJ TechTools and Mixed In Key blog and forums, Pioneer DJ forum, the Algoriddim (djay) forum, VirtualDJ forum, Hacker News, and the Apple Developer Forums.
- YouTube: search "AutoMix iOS 26 spectrogram", "AutoMix vs DJ", "AutoMix iOS 27 test". Transcripts and pinned comments often contain the technique names. Note video title, channel and timestamp.

**Primary and semi-primary:**
- https://support.apple.com/guide/iphone/transition-songs-iphadf2fe1f4/ios and https://support.apple.com/en-us/105067 (device list, and any statement on where analysis runs)
- WWDC25 and WWDC26 session lists, Apple Newsroom (June 2025 and June 2026), and iOS 27 release notes for any AutoMix technical sentence.
- Apple patents and applications: Google Patents and USPTO for Apple Inc. filings on song transition, beat-matched crossfade, and tempo-adjusted playback. Search terms include "transition between audio tracks", "beat alignment", "playback rate", "loop intro outro". Give patent number, filing date and the passage.
- https://github.com/chenqi92/primuse/issues/117 (already read; only the MusicKit limitation matters).

**Secondary, low trust** (AppleMagazine, podcastvideos.com, Mac Observer, TechPulse) are the origin of the unverified "entirely on device, Neural Engine" claim. Trace it to an Apple statement, or record that no primary source exists.

### 8.4 If you can get audio

Best evidence is a recording. If a device with an Apple Music subscription is available, or someone has posted clean captures (line-in, or a screen recording with audio), analyse them rather than relying on prose. Do this without touching the app:

1. Capture 10 or more transitions, mixed genres, iOS 26 and 27 if possible. Keep the raw files out of the repo (size); record song pairs and OS version in the notes.
2. Per transition, measure: overlap length; per-band level of the outgoing and incoming song over time (low, mid, high), which shows a bass swap or filter sweep; tempo of each song before, during and after (a ramp or a step); loop or repeat detection (self-similarity of the outgoing tail); position of the mix relative to the beat grid.
3. The repo's own harness, `crates/core/src/automix/tests.rs` and the sections 7.3 harness described above, already scores our mixes on similar quantities. Reuse its measures so Apple and nori numbers are comparable.

### 8.5 Deliverable

Update this file in place; don't create a new one.

- Replace section 1a with the corrected findings, keeping the labels. Move settled items up into section 1.
- Add one table: transition technique, source, evidence label, iOS version. One row per finding.
- List the song pairs and quotes that carry the strongest evidence, each with its URL. Keep quotes short and exact.
- Mark every claim you couldn't source **[unknown]**. Don't fill gaps with plausible DSP guesses; section 1 already has those, labelled **[inferred]**.
- End with "What this changes for nori": at most 5 bullets, each tied to a step in `automix-next.md` §3 (loops, tempo ramps, natural blends, and so on), so a builder can act on it. Don't edit the design sections yourself; propose changes.
- Add each new source URL to `automix-next.md` §5.

### 8.6 Constraints

- Follow `AGENTS.md`. Docs only in this task; no code changes.
- Cite a URL for every **[verified]** claim. If a page couldn't be opened, don't cite it as read.
- Nothing from a paywalled or private page beyond a short quote.
- If a site is blocked in your environment too, list it under "Could not open" at the end of section 1a, so the next person knows.
