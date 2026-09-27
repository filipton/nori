package dev.nori.music.app.ui

import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import androidx.compose.runtime.collectAsState
import android.content.Intent
import android.net.Uri
import androidx.compose.ui.platform.LocalContext
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.statusBarsPadding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.LibraryMusic
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.Surface
import androidx.compose.material3.SnackbarHostState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.material3.MaterialTheme
import androidx.compose.foundation.background
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.animation.core.animateFloat
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.Alignment
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import dev.nori.music.app.vm.ActionsViewModel
import dev.nori.music.app.vm.PlayerViewModel
import dev.nori.music.app.vm.SettingsViewModel
import dev.nori.music.ffi.model.Song
import dev.nori.music.settings.loggedIn

/** Plain screens sit below the status bar; album, artist and playlist pages draw under it. */
@Composable
private fun Inset(content: @Composable () -> Unit) = androidx.compose.foundation.layout.Box(Modifier.statusBarsPadding()) { content() }

/**
 * Navigation as the screens see it; they never touch the NavController. Going anywhere puts the player
 * away first, the way an artist tapped on Apple's player drops it and opens the artist underneath.
 */
class Nav(private val c: NavHostController, private val sheet: PlayerSheet) {
    fun go(route: String) { if (sheet.isOpen) sheet.close(); c.navigate(route) }

    /**
     * What the row that opened an album already knew about it - name, artist, cover - kept for the page
     * to draw its header from while the album itself is on the way. The page slides in (PageMotion)
     * the moment it is asked for, and without this it slid in as a bare card and filled in a few frames
     * later; with it the header is there from the first frame and only the songs arrive. The last few
     * are kept, so going back and forward between albums does not lose them.
     */
    private val albums = object : LinkedHashMap<String, dev.nori.music.ffi.model.Album>() {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, dev.nori.music.ffi.model.Album>?) = size > 8
    }
    private val artists = object : LinkedHashMap<String, dev.nori.music.ffi.model.Artist>() {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, dev.nori.music.ffi.model.Artist>?) = size > 8
    }
    private val playlists = object : LinkedHashMap<String, dev.nori.music.ffi.model.Playlist>() {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, dev.nori.music.ffi.model.Playlist>?) = size > 8
    }
    fun albumHint(id: String): dev.nori.music.ffi.model.Album? = albums[id]
    fun artistHint(id: String): dev.nori.music.ffi.model.Artist? = artists[id]
    fun playlistHint(id: String): dev.nori.music.ffi.model.Playlist? = playlists[id]

    fun album(id: String, hint: dev.nori.music.ffi.model.Album? = null) {
        if (hint != null) albums[id] = hint
        go("album/${Uri.encode(id)}")
    }
    fun artist(id: String, hint: dev.nori.music.ffi.model.Artist? = null) {
        if (hint != null) artists[id] = hint
        go("artist/${Uri.encode(id)}")
    }
    fun playlist(id: String, hint: dev.nori.music.ffi.model.Playlist? = null) {
        if (hint != null) playlists[id] = hint
        go("playlist/${Uri.encode(id)}")
    }
    fun genre(name: String) = go("genre/${Uri.encode(name)}")
    fun folder(id: String) = go("folder/${Uri.encode(id)}")
    fun decade(year: Int) = go("decade/$year")
    fun smart(id: String) = go("smart/${Uri.encode(id)}")
    fun mix(id: String) = go("mix/${Uri.encode(id)}")
    fun smartEdit(id: String) = go("smartEdit/${Uri.encode(id.ifEmpty { "new" })}")
    fun stats() = go("stats")
    /** The download queue. Asked for again while it is showing (the notification tapped), it only puts the player away. */
    fun downloads() { if (c.currentDestination?.route == "downloads") { if (sheet.isOpen) sheet.close() } else go("downloads") }
    /** A settings group, optionally landing on one row of it (from the settings search). */
    fun settingsGroup(id: String, key: String = "") = go("settings/$id?key=${Uri.encode(key)}")
    fun player() = sheet.open()
    fun equalizer() = go("equalizer")
    fun autoEq() = go("autoeq")
    fun back() { if (sheet.isOpen) sheet.close() else c.popBackStack() }
    /**
     * A tab always lands on that tab's own page. It used to save the stack it popped and restore it on
     * the way back, which meant tapping Home from an album popped the album and then put it straight
     * back - the tab looked dead. Nothing above the tab roots survives a tab tap now.
     */
    fun tab(route: String) {
        if (sheet.isOpen) sheet.close()
        // Tapping Search is a request for the keyboard, whether or not the screen is already open -
        // and with launchSingleTop it is not recomposed, so nothing else would notice the tap.
        if (route == "search") searchTaps.intValue++
        // Already there: nothing to go to. Navigating anyway ran the page's arrival again on every tap,
        // so a tab pressed twice redrew the page it was already on.
        if (c.currentBackStackEntry?.destination?.route == route) return
        c.navigate(route) {
            popUpTo(c.graph.startDestinationId)
            launchSingleTop = true
        }
    }
}

/** Counts taps on the search tab; the search field takes focus again on each one. */
private val searchTaps = androidx.compose.runtime.mutableIntStateOf(0)

@Composable
internal fun searchFocusKey(): Int = searchTaps.intValue

val LocalNav = staticCompositionLocalOf<Nav> { error("no nav") }
val LocalSongMenu = staticCompositionLocalOf<(Song) -> Unit> { {} }

/** The player's own ⋯: the same song menu, with the playback-wide entries the player needs. */
val LocalPlayerMenu = staticCompositionLocalOf<(Song) -> Unit> { {} }

private val tabs = listOf(
    Tab("home", say.home, Icons.Filled.Home),
    Tab("search", say.search, Icons.Filled.Search),
    Tab("library", say.library, Icons.Filled.LibraryMusic),
    Tab("settings", say.settings, Icons.Filled.Settings),
)

/**
 * [launchRoute] is what the activity was asked for from outside - a tap on the download notification, or
 * a launcher shortcut: a screen, or a shuffle to start - set on launch or on a new intent, and cleared
 * here once it has been done.
 */
@Composable
fun App(launchRoute: androidx.compose.runtime.MutableState<String?>? = null) {
    val settings: SettingsViewModel = viewModel()
    val prefs by settings.prefs.collectAsStateWithLifecycle()
    androidx.compose.runtime.SideEffect { AppMotion.force = prefs.ignoreSystemMotion; AppMotion.reduce = prefs.reduceMotion }
    NoriTheme(prefs) {
        // Sign-in used to cut straight to the app. One short fade is enough: the screens are different
        // enough that a direction would invent a relationship they do not have.
        val plain = reduceMotion()
        androidx.compose.animation.Crossfade(
            targetState = prefs.loggedIn,
            animationSpec = if (plain) androidx.compose.animation.core.snap() else androidx.compose.animation.core.tween(280),
            label = "session",
        ) { loggedIn ->
        if (!loggedIn) {
            LoginScreen(settings)
            return@Crossfade
        }

        val controller = rememberNavController()
        val sheetScope = androidx.compose.runtime.rememberCoroutineScope()
        val sheet = remember { PlayerSheet(sheetScope) }
        sheet.plain = reduceMotion()
        // Whether the player was open, kept with the rest of the saved state, so an app Android put back
        // together (after it was stopped in the background) opens on the player it was left on.
        var playerWasOpen by androidx.compose.runtime.saveable.rememberSaveable { mutableStateOf(false) }
        LaunchedEffect(sheet) {
            if (playerWasOpen) sheet.openAtOnce()
            androidx.compose.runtime.snapshotFlow { sheet.isOpen }.collect { playerWasOpen = it }
        }
        val nav = remember(controller) { Nav(controller, sheet) }
        val actions: ActionsViewModel = viewModel()
        val player: PlayerViewModel = viewModel()
        val snackbar = remember { SnackbarHostState() }
        var menuSong by remember { mutableStateOf<Song?>(null) }
        var menuFromPlayer by remember { mutableStateOf(false) }
        LaunchedEffect(Unit) {
            // Latest, not in turn: showing a message suspends until it goes away, so a plain collect
            // could not even see the next one until the last had sat out its four seconds - a quick
            // favourite then unfavourite read "Added to favourites" for the whole of that. Cancelling
            // the one that is up takes it off at once and the new one replaces it.
            actions.messages.collectLatest { msg ->
                snackbar.currentSnackbarData?.dismiss()
                snackbar.showSnackbar(msg, withDismissAction = true, duration = androidx.compose.material3.SnackbarDuration.Short)
            }
        }
        // Headphones or a DAC connected with nothing chosen for them: the service decided (see DeviceSound);
        // this only says so - "use its AutoEQ curve?", or "using AutoEQ for X" with an undo.
        // Marked as seen only once it has been on screen; unplugging the device takes it away with it.
        val eqNotice by settings.eqNotice.collectAsStateWithLifecycle()
        LaunchedEffect(eqNotice) {
            val n = eqNotice ?: return@LaunchedEffect
            // An offer waits for an answer; a curve already applied only offers to undo it.
            val offer = n.source is dev.nori.music.playback.DeviceSound.Offer
            val result = snackbar.showSnackbar(
                n.message, actionLabel = n.action, withDismissAction = offer,
                duration = if (offer) androidx.compose.material3.SnackbarDuration.Long else androidx.compose.material3.SnackbarDuration.Short,
            )
            settings.eqNoticeShown(n)
            if (result == androidx.compose.material3.SnackbarResult.ActionPerformed) settings.eqNoticeAction(n)
        }
        val context = LocalContext.current
        LaunchedEffect(Unit) {
            actions.shares.collect { url ->
                context.startActivity(Intent.createChooser(Intent(Intent.ACTION_SEND).setType("text/plain").putExtra(Intent.EXTRA_TEXT, url), null))
            }
        }

        // The debug build's test bridge drives the app through this (src/debug TestDriver.kt); every
        // other build has an empty one (src/noTest), so a release carries none of it.
        dev.nori.music.app.TestDriver(controller, nav, sheet, settings, actions, player)

        // Whether the beat model also reads what is being downloaded, when the settings say to ask.
        BeatsQuestion(actions)

        // The perf build's recorder starts a new stretch when the player goes up or away. See PerfHooks.
        dev.nori.music.app.PerfHooks.recorder?.let { r ->
            LaunchedEffect(sheet, r) { androidx.compose.runtime.snapshotFlow { sheet.isOpen }.collect(r::playerOpen) }
        }

        // Read from a snapshot observer, not in composition, so a request does not recompose the app.
        if (launchRoute != null) LaunchedEffect(launchRoute, nav) {
            androidx.compose.runtime.snapshotFlow { launchRoute.value }.collect { route ->
                if (route == null) return@collect
                when (route) {
                    "downloads" -> nav.downloads()
                    // A tab, as a tap on it: the keyboard comes up with it.
                    "search" -> nav.tab(route)
                    dev.nori.music.app.SHUFFLE_SONGS -> actions.shuffleAll()
                    dev.nori.music.app.SHUFFLE_ALBUMS -> actions.shuffleAlbums()
                    else -> nav.go(route)
                }
                launchRoute.value = null
            }
        }

        CompositionLocalProvider(
            LocalNav provides nav,
            LocalPlayerSheet provides sheet,
            LocalDownloadMarks provides rememberDownloadMarks(actions),
            LocalSongMenu provides { menuSong = it; menuFromPlayer = false },
            LocalPlayerMenu provides { menuSong = it; menuFromPlayer = true },
        ) {
            val route = controller.currentBackStackEntryAsState().value?.destination?.route
            // The tab the page on screen belongs to, which is the one that stays lit, as Apple's does: a
            // settings group, or an album opened from Home, is still inside that tab. Lit only on the tab
            // roots themselves, the icon went out the moment anything was opened. There is one back stack
            // and a tab tap rebuilds it from the start, so the page's tab is the last root shown.
            var lastTab by androidx.compose.runtime.saveable.rememberSaveable { mutableStateOf("home") }
            val onTab = route?.takeIf { r -> tabs.any { it.route == r } }
            LaunchedEffect(onTab) { if (onTab != null) lastTab = onTab }
            val tabRoute = onTab ?: lastTab
            // This session's star changes, so every heart prefers them over the snapshot it painted with.
            val marks by actions.starMarks.collectAsStateWithLifecycle()
            // The chrome floats over the page rather than ending it: the page fills the window, its colour
            // reaches the bottom edge, and the list scrolls under the mini player the way Apple's does.
            // Screens keep the last row reachable by adding LocalChromeInset to their content padding.
            var chromeHeight by remember { mutableStateOf(0.dp) }
            var tabsHeight by remember { mutableStateOf(0.dp) }
            // On its side the tabs stand on a rail at the left edge (TabRail), and the page gives it that edge.
            val config = androidx.compose.ui.platform.LocalConfiguration.current
            val wide = isWide(config.screenWidthDp, config.screenHeightDp)
            var railWidth by remember { mutableStateOf(0.dp) }
            val railInset = if (wide) railWidth else 0.dp
            val density = androidx.compose.ui.platform.LocalDensity.current
            // One look for both halves of the chrome, cross-fading once when the page under it changes.
            val chromeLook = rememberChromeLook()
            // This replaced a Scaffold when the chrome started floating, and with it went the two things
            // Scaffold quietly provided: something that paints the app's background (every screen was
            // showing the window's default grey, lighter than our own cards) and a content colour for
            // text that does not name one (which left titles rendering almost black).
            Surface(color = MaterialTheme.colorScheme.background, contentColor = MaterialTheme.colorScheme.onBackground) {
            Box(Modifier.fillMaxSize().onGloballyPositioned { sheet.rootHeight = it.size.height.toFloat() }) {
              // Everything under the player. Once the player covers it completely it is not drawn at all:
              // a layer at zero alpha is skipped, so a page left animating underneath costs nothing.
              CompositionLocalProvider(LocalWide provides wide) {
              Box(Modifier.fillMaxSize().graphicsLayer { alpha = if (sheet.progress.value >= 1f) 0f else 1f }) {
              // The strip the rail stands on wears the page's colour, so a tinted page runs to the edge.
              if (wide) Box(Modifier.fillMaxHeight().width(railInset).drawBehind { drawRect(chromeLook.color(dev.nori.music.look.CoverLook.CHROME_PAGE)) })
              CompositionLocalProvider(LocalStarMarks provides marks, LocalChromeInset provides chromeHeight) {
              Box(Modifier.fillMaxSize().padding(start = railInset)) {
                // One transition for the whole app, and the same one in both directions. See PageMotion.
                val plain = reduceMotion()
                NavHost(
                    controller, "home",
                    enterTransition = { PageMotion.enter(this, plain) },
                    exitTransition = { PageMotion.exit(this, plain) },
                    popEnterTransition = { PageMotion.popEnter(this, plain) },
                    popExitTransition = { PageMotion.popExit(this, plain) },
                    // The back gesture scrubs these with the finger. Left to the library's defaults, the page
                    // being left shrank to 70 % without fading, over a page that was already fully drawn -
                    // two pages on top of each other for the whole gesture. See PageMotion.
                    predictivePopEnterTransition = { _ -> PageMotion.popEnter(this, plain, scrubbed = true) },
                    predictivePopExitTransition = { _ -> PageMotion.popExit(this, plain, scrubbed = true) },
                ) {
                    page("home") { HomeScreen(actions) }
                    page("search") { SearchScreen(actions) }
                    page("library") { LibraryScreen(actions) }
                    page("settings") { SettingsScreen(settings) }
                    page("settings/{id}?key={key}") { e ->
                        SettingsGroupScreen(settings, e.arguments!!.getString("id")!!, e.arguments?.getString("key").orEmpty(), actions)
                    }
                    page("equalizer") { EqualizerScreen(settings) }
                    page("autoeq") { AutoEqScreen(settings) }
                    page("album/{id}", inset = false) { AlbumScreen(it.arguments!!.getString("id")!!, actions) }
                    page("artist/{id}", inset = false) { ArtistScreen(it.arguments!!.getString("id")!!, actions) }
                    page("playlist/{id}", inset = false) { PlaylistScreen(it.arguments!!.getString("id")!!, actions) }
                    page("mix/{id}", inset = false) { MixScreen(it.arguments!!.getString("id")!!, actions) }
                    page("genre/{id}") { GenreScreen(it.arguments!!.getString("id")!!, actions) }
                    page("smart/{id}") { SmartScreen(it.arguments!!.getString("id")!!, actions) }
                    page("smartEdit/{id}") { SmartEditScreen(it.arguments!!.getString("id")!!.let { i -> if (i == "new") "" else i }) }
                    page("stats") { StatsScreen() }
                    page("perf") { dev.nori.music.app.PerfHooks.recorder?.Page() }
                    page("downloads") { DownloadsScreen(actions) }
                    page("folder/{id}") { FolderScreen(it.arguments!!.getString("id")!!, actions) }
                    page("decade/{year}") { SongsScreen(actions, it.arguments!!.getString("year")!!.toInt()) }
                }
                // A newer release, and its download and install once asked for; under the player, so the
                // player covers it. See UpdateBanner.
                UpdateBanner(settings, Modifier.align(Alignment.TopCenter))
              }
              }
              Box(
                  Modifier.align(Alignment.BottomCenter).padding(start = railInset)
                      .onGloballyPositioned { chromeHeight = with(density) { it.size.height.toDp() } },
              ) {
                  // The now playing bar has a heart now, and it reads the stars the user has just
                  // changed from here like every other one. Outside this, it saw only the server's
                  // answer, so a tap on it changed nothing until the song came round again.
                  CompositionLocalProvider(LocalStarMarks provides marks) { BottomChrome(player, actions, nav::player, if (wide) 0.dp else tabsHeight, chromeLook) }
              }
              }
              PlayerLayer(sheet) { CompositionLocalProvider(LocalStarMarks provides marks) { PlayerScreen(player, actions) } }
              // The tab bar is over the player, not under it: as the player rises it slides down off the
              // screen instead of vanishing under the sheet in one frame. See BottomChrome.
              if (wide) Box(Modifier.align(Alignment.CenterStart).fillMaxHeight()) { TabRail(tabRoute, tabs, nav::tab, chromeLook, player) { railWidth = it } }
              else Box(Modifier.align(Alignment.BottomCenter)) { TabBar(tabRoute, tabs, nav::tab, chromeLook, player) { tabsHeight = it } }
              }
              // Top: less in the way of the now-playing bar; swipe or the X dismisses.
              SnackbarHost(
                  snackbar,
                  Modifier.align(Alignment.TopCenter).statusBarsPadding().padding(top = 8.dp, start = 12.dp, end = 12.dp),
              ) { data ->
                  val dismiss = androidx.compose.material3.rememberSwipeToDismissBoxState(
                      confirmValueChange = {
                          if (it != androidx.compose.material3.SwipeToDismissBoxValue.Settled) {
                              data.dismiss()
                              true
                          } else false
                      },
                  )
                  androidx.compose.material3.SwipeToDismissBox(
                      state = dismiss,
                      backgroundContent = {},
                      enableDismissFromStartToEnd = true,
                      enableDismissFromEndToStart = true,
                  ) {
                      androidx.compose.material3.Snackbar(snackbarData = data)
                  }
              }
            }
            SheetBack(sheet)
            SelectionBack(actions, sheet, controller)
            }
            SongMenu(menuSong, actions, onDismiss = { menuSong = null }, player = player.takeIf { menuFromPlayer })
        }
        }
    }
}

/**
 * Back puts the player away while it is up. Registered after the NavHost's own, so it goes first; and
 * its own small scope, so the sheet opening and closing recomposes this and not the whole app.
 */
@Composable
private fun SheetBack(sheet: PlayerSheet) {
    // With the back gesture the sheet sinks a little with the finger, the way a page does, and goes
    // on down if the gesture is let go, or back up if it is called off.
    androidx.activity.compose.PredictiveBackHandler(sheet.isOpen) { events ->
        try {
            events.collect { sheet.backBy(it.progress) }
            sheet.backClose()
        } catch (e: kotlinx.coroutines.CancellationException) {
            sheet.open()
            throw e
        }
    }
}

/**
 * Back while songs are selected lets go of them and stays on the page; registered after the NavHost's
 * own, so it goes first. Not while the player is up: back puts the player away first. And any change of
 * page - back, a tap, a tab, a link - ends the selection, so the bar never outlives the list it was for.
 */
@Composable
private fun SelectionBack(actions: ActionsViewModel, sheet: PlayerSheet, controller: NavHostController) {
    val selecting by remember(actions) { actions.selection.map { it.isNotEmpty() }.distinctUntilChanged() }.collectAsState(actions.selection.value.isNotEmpty())
    androidx.activity.compose.BackHandler(selecting && !sheet.isOpen) { actions.backFromSelection() }
    LaunchedEffect(controller, actions) { controller.currentBackStackEntryFlow.collect { actions.onPage(it.id) } }
}

/**
 * One route of the app: its content, wrapped so that it takes part in [PageMotion]. [inset] puts plain
 * screens below the status bar; album, artist and playlist pages draw under it.
 */
private fun androidx.navigation.NavGraphBuilder.page(
    route: String, inset: Boolean = true,
    content: @Composable (androidx.navigation.NavBackStackEntry) -> Unit,
) = composable(route) { entry -> Page { if (inset) Inset { content(entry) } else content(entry) } }

/**
 * The pages are a stack, and they move the way a stack of cards does - the way iOS pushes a page.
 *
 * Push: the new page slides in from the right edge, opaque, the whole width, and lands over the page
 * that opened it; that page slides a third of the way off to the left underneath and darkens a little,
 * so it is plainly still there, behind. Pop - the back button, or the back gesture with the finger
 * doing the scrubbing - is the same in reverse: the page on top slides off to the right and the one
 * underneath comes back to its place and to full light.
 *
 * Nothing fades. The pages are opaque and stay opaque; the only alpha here is the scrim on the page
 * underneath. Two earlier versions did fade: a sideways slide with a fade on top, which the owner read
 * as the page flying out of the top left corner, and then a drop from a little above the page's place
 * with both pages fading, which for a few frames left neither page opaque and the window's black
 * showing through - "everything animates from the top of the page". A stack has a direction the eye
 * already knows, and a card that is opaque cannot fly diagonally.
 *
 * Between two tab roots there is no stack - Home and Library are siblings - so that change is a
 * plain, short cross-fade with no direction and no scrim (Apple Music does not animate it at all).
 *
 * The page underneath is dimmed by [Page], which reads the transition it is part of; which side of the
 * stack a page is on during a change is written here, where the direction is known, and read there.
 */
private object PageMotion {
    /** How long a button push or pop takes. The gesture sets its own pace while the finger is down. */
    const val MS = 200
    /**
     * After the back gesture lets go (finish or cancel): only the leftover distance runs, and it
     * should be gone almost at once. Reusing [MS] made a half-done swipe still coast for a beat.
     */
    private const val GESTURE_MS = 140
    /** The share of the width the page underneath moves. */
    const val UNDER = 3
    /** How dark the page underneath goes at the far end of its travel. */
    const val SCRIM = 0.28f
    /** A tab change: a cross-fade this long. */
    private const val TAB_MS = 100

    /** Sharp settle for a tap: travel early, short ease into place. */
    val Settle = androidx.compose.animation.core.CubicBezierEasing(0.2f, 0f, 0f, 1f)

    private val roots = setOf("home", "search", "library", "settings")

    /**
     * True while the change under way is a pop, false for a push, null for a tab change or none. Read
     * by every page in flight to decide which of the two it is: the entering page of a push and the
     * leaving page of a pop are on top; the other two are underneath and wear the scrim.
     */
    var pop: Boolean? by mutableStateOf(null)
        private set
    /** The pop under way is the back gesture's, so it runs linear in time: the finger sets the pace. */
    var scrubbed: Boolean by mutableStateOf(false)
        private set

    private fun androidx.compose.animation.AnimatedContentTransitionScope<androidx.navigation.NavBackStackEntry>.tab(): Boolean =
        initialState.destination.route in roots && targetState.destination.route in roots

    private fun ease(scrubbed: Boolean): androidx.compose.animation.core.FiniteAnimationSpec<androidx.compose.ui.unit.IntOffset> =
        if (scrubbed) androidx.compose.animation.core.tween(GESTURE_MS, easing = androidx.compose.animation.core.LinearEasing)
        else androidx.compose.animation.core.tween(MS, easing = Settle)

    private fun fadeIn(ms: Int) = androidx.compose.animation.fadeIn(androidx.compose.animation.core.tween(ms))
    private fun fadeOut(ms: Int) = androidx.compose.animation.fadeOut(androidx.compose.animation.core.tween(ms))

    private fun androidx.compose.animation.AnimatedContentTransitionScope<androidx.navigation.NavBackStackEntry>.begin(pop: Boolean, scrubbed: Boolean): Boolean {
        val tab = tab()
        PageMotion.pop = if (tab) null else pop
        PageMotion.scrubbed = scrubbed && !tab
        return tab
    }

    fun enter(s: androidx.compose.animation.AnimatedContentTransitionScope<androidx.navigation.NavBackStackEntry>, plain: Boolean): androidx.compose.animation.EnterTransition {
        if (s.begin(pop = false, scrubbed = false)) return fadeIn(if (plain) 90 else TAB_MS)
        if (plain) return fadeIn(90)
        return androidx.compose.animation.slideInHorizontally(ease(false)) { it }
    }

    fun exit(s: androidx.compose.animation.AnimatedContentTransitionScope<androidx.navigation.NavBackStackEntry>, plain: Boolean): androidx.compose.animation.ExitTransition {
        if (s.begin(pop = false, scrubbed = false)) return fadeOut(if (plain) 90 else TAB_MS)
        if (plain) return fadeOut(90)
        return androidx.compose.animation.slideOutHorizontally(ease(false)) { -it / UNDER }
    }

    fun popEnter(s: androidx.compose.animation.AnimatedContentTransitionScope<androidx.navigation.NavBackStackEntry>, plain: Boolean, scrubbed: Boolean = false): androidx.compose.animation.EnterTransition {
        if (s.begin(pop = true, scrubbed)) return fadeIn(if (plain) 90 else TAB_MS)
        if (plain) return fadeIn(90)
        return androidx.compose.animation.slideInHorizontally(ease(scrubbed)) { -it / UNDER }
    }

    fun popExit(s: androidx.compose.animation.AnimatedContentTransitionScope<androidx.navigation.NavBackStackEntry>, plain: Boolean, scrubbed: Boolean = false): androidx.compose.animation.ExitTransition {
        if (s.begin(pop = true, scrubbed)) return fadeOut(if (plain) 90 else TAB_MS)
        if (plain) return fadeOut(90)
        return androidx.compose.animation.slideOutHorizontally(ease(scrubbed)) { it }
    }

    /** How dark a page in [state] is, given which way the stack is moving: only the page underneath is dimmed. */
    fun dim(state: androidx.compose.animation.EnterExitState, pop: Boolean?): Float = when {
        pop == null || state == androidx.compose.animation.EnterExitState.Visible -> 0f
        // Leaving during a push: being covered. Entering during a pop: being uncovered.
        state == androidx.compose.animation.EnterExitState.PostExit && !pop -> SCRIM
        state == androidx.compose.animation.EnterExitState.PreEnter && pop -> SCRIM
        else -> 0f
    }

    fun dimSpec(plain: Boolean, scrubbed: Boolean): androidx.compose.animation.core.FiniteAnimationSpec<Float> = when {
        plain -> androidx.compose.animation.core.tween(90)
        scrubbed -> androidx.compose.animation.core.tween(GESTURE_MS, easing = androidx.compose.animation.core.LinearEasing)
        else -> androidx.compose.animation.core.tween(MS, easing = Settle)
    }
}

/**
 * A page as [PageMotion] needs it: opaque while it moves, and dimmed when it is the one underneath.
 *
 * Opaque because the screens themselves paint no background - the app's one Surface does - so a page
 * sliding in would otherwise show the page it is covering through itself. The background is drawn
 * only while the page is in a transition; at rest the Surface's is the one that shows, as before.
 * The scrim is a child of the page's own transition, so the back gesture scrubs it with the slide and
 * it runs back if the gesture is called off.
 */
@Composable
private fun androidx.compose.animation.AnimatedVisibilityScope.Page(content: @Composable () -> Unit) {
    val plain = reduceMotion()
    val dim by transition.animateFloat(
        transitionSpec = { PageMotion.dimSpec(plain, PageMotion.scrubbed) },
        label = "dim",
    ) { state -> PageMotion.dim(state, PageMotion.pop) }
    val background = MaterialTheme.colorScheme.background
    Box(
        Modifier.fillMaxSize()
            .drawBehind { if (transition.currentState != transition.targetState) drawRect(background) }
            .drawWithContent { drawContent(); if (dim > 0.002f) drawRect(androidx.compose.ui.graphics.Color.Black.copy(alpha = dim)) },
    ) { content() }
}

/**
 * The player sheet over everything else, present only while it is open or moving. The page behind
 * darkens as it rises; the sheet's top corners round off while it is in flight and square up once it
 * fills the screen. All of it is read in the draw phase, so a drag redraws two layers and recomposes
 * nothing.
 */
@Composable
private fun PlayerLayer(sheet: PlayerSheet, content: @Composable () -> Unit) {
    val shown by remember { androidx.compose.runtime.derivedStateOf { sheet.progress.value > 0f || sheet.progress.targetValue > 0f } }
    // Built once, shortly after the app is up, and kept: opening the player then only moves it. Until
    // then, and on a phone too slow to have got there, the first open builds it.
    var warm by remember { mutableStateOf(false) }
    LaunchedEffect(Unit) { kotlinx.coroutines.delay(1500); warm = true }
    if (!shown && !warm) return
    val radius = with(androidx.compose.ui.platform.LocalDensity.current) { 14.dp.toPx() }
    if (shown) Box(
        Modifier.fillMaxSize().drawBehind {
            drawRect(androidx.compose.ui.graphics.Color.Black.copy(alpha = 0.45f * sheet.progress.value))
        },
    )
    Box(
        Modifier.fillMaxSize().graphicsLayer {
            // Put away, it is parked a whole screen below the bottom edge: nothing drawn, and nothing to
            // catch a touch meant for the mini player (a transparent layer would still be hit).
            val parked = sheet.progress.value == 0f && sheet.progress.targetValue == 0f
            translationY = if (parked) sheet.rootHeight * 2f + 1f else sheet.offset()
            // It comes up *through* the now playing bar rather than on top of it: for the first tenth of
            // the rise the player is still part transparent, so the bar shows through its own place in
            // it. Snapping to full strength on the first frame of a drag - which is what this did - made
            // a whole dark page appear out of nothing before it had moved anywhere.
            alpha = if (parked) 0f else (sheet.progress.value / 0.1f).coerceIn(0f, 1f)
            val r = radius * (1f - sheet.progress.value).coerceIn(0f, 1f) * 4f
            shape = androidx.compose.foundation.shape.RoundedCornerShape(topStart = r.coerceAtMost(radius), topEnd = r.coerceAtMost(radius))
            clip = true
        }
            // The player is a page, not a pane of glass. Only its buttons and gestures listened, so a
            // tap anywhere else on it - between the lyrics, beside the artwork - fell through to what
            // is underneath: the list the player was opened from, playing a song nobody could see.
            // Being hit at all is enough to keep a touch here; nothing is consumed, so every control
            // inside works as before.
            .pointerInput(Unit) { awaitPointerEventScope { while (true) awaitPointerEvent() } },
    ) {
        CompositionLocalProvider(LocalChromeInset provides 0.dp, LocalPlayerShown provides shown) { content() }
    }
}
