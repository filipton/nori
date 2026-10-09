import java.util.Properties

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.compose.compiler)
}

android {
    namespace = "dev.nori.music.app"
    // The car's own API, for where its driver sits (ui/DriverSide.kt): present only on a car, and asked for there only.
    useLibrary("android.car", false)
    compileSdk = 37

    defaultConfig {
        applicationId = "dev.nori.music"
        minSdk = 26
        targetSdk = 36
        versionName = "0.6.0-beta.2"
        versionCode = 60002
        // For checking the updater only: `-PpretendVersion=0.3.9` builds this as that older version (its code as
        // bump-version.sh makes it), so it finds the latest GitHub release newer and can install it in place.
        (project.findProperty("pretendVersion") as String?)?.let { v ->
            val (major, minor, patch) = v.substringBefore('-').split(".").map { it.toInt() }
            val beta = v.substringAfter("-beta.", "").toIntOrNull() ?: 99
            versionName = v
            versionCode = major * 1000000 + minor * 10000 + patch * 100 + beta
        }
        ndk { abiFilters += (project.findProperty("rustTargets") as String? ?: "arm64-v8a,x86_64").split(",") }
        // What About can say about this build beyond a version number: the commit it was cut from, and
        // the versions of what it is built on - the Rust crates read from Cargo.lock, the Android ones
        // from the version catalog - so they cannot drift from what was actually linked.
        val gitSha = providers.exec {
            workingDir = rootProject.projectDir
            commandLine("git", "rev-parse", "--short=10", "HEAD")
            isIgnoreExitValue = true
        }.standardOutput.asText.map { it.trim() }.getOrElse("")
        buildConfigField("String", "GIT_SHA", "\"$gitSha\"")
        val lock = rootProject.file("Cargo.lock").takeIf { it.exists() }?.readText().orEmpty()
        fun locked(crate: String): String =
            Regex("name = \"" + Regex.escape(crate) + "\"\nversion = \"([^\"]+)\"").find(lock)?.groupValues?.get(1) ?: ""
        val rust = listOf("uniffi", "rusqlite", "rustfft", "signalsmith-stretch", "serde", "jni")
            .joinToString(";") { "$it=${locked(it)}" }
        val catalog = rootProject.file("gradle/libs.versions.toml").takeIf { it.exists() }?.readText().orEmpty()
        fun cat(key: String): String = Regex("(?m)^" + Regex.escape(key) + "\\s*=\\s*\"([^\"]+)\"").find(catalog)?.groupValues?.get(1) ?: ""
        val android = listOf("media3", "composeBom", "okhttp").joinToString(";") { "$it=${cat(it)}" }
        buildConfigField("String", "CORE_VERSIONS", "\"$rust;$android\"")
    }

    // Release signing: keystore.properties in the repo root (tools/release.sh creates one, with
    // nori-release.jks, on its first run), or KEYSTORE_FILE / KEYSTORE_PASSWORD / KEY_ALIAS /
    // KEY_PASSWORD in the environment. Without either it falls back to the debug key, so
    // `assembleRelease` always gives an installable APK - but one that cannot update a release build.
    val ksProps = Properties().apply {
        val f = rootProject.file("keystore.properties")
        if (f.exists()) f.inputStream().use { load(it) }
    }
    fun ks(name: String, env: String): String? = ksProps.getProperty(name) ?: System.getenv(env)
    val ksFile = ks("storeFile", "KEYSTORE_FILE")?.let { rootProject.file(it) }
    if (ksFile != null && ksFile.exists()) {
        signingConfigs {
            create("release") {
                storeFile = ksFile
                storePassword = ks("storePassword", "KEYSTORE_PASSWORD")
                keyAlias = ks("keyAlias", "KEY_ALIAS") ?: "nori"
                keyPassword = ks("keyPassword", "KEY_PASSWORD") ?: ks("storePassword", "KEYSTORE_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"))
            signingConfig = signingConfigs.findByName("release") ?: signingConfigs.getByName("debug")
        }
        // A release build with the recorder in it, for measuring on a real phone without adb
        // (docs/perf-build.md). Minified and not debuggable like a release, so what it measures is what
        // a release costs; installed beside the normal app under its own id and name.
        create("perf") {
            initWith(getByName("release"))
            applicationIdSuffix = ".perf"
            isDebuggable = false
            signingConfig = signingConfigs.getByName("debug")
            matchingFallbacks += "release"
        }
    }

    // The benchmarks: run over adb in a debug build (TestBridge), from the Performance page in a perf build.
    sourceSets {
        getByName("debug").kotlin.srcDir("src/bench/kotlin")
        getByName("perf").kotlin.srcDir("src/bench/kotlin")
        // The test bridge (TestBridge over adb, what it drives) is the debug build's alone (src/debug); the
        // release and perf builds get its empty twin, so they carry none of it. The perf build's self test
        // drives the app by itself and needs none of the bridge.
        getByName("release").kotlin.srcDir("src/noTest/kotlin")
        getByName("perf").kotlin.srcDir("src/noTest/kotlin")
        // The self test's plain logic (no Android in it): built into the perf build, and tested on the JVM
        // with the unit tests, which AGP runs for the debug build only.
        getByName("perf").kotlin.srcDir("src/perf/logic")
        getByName("test").kotlin.srcDir("src/perf/logic")
        getByName("test").kotlin.srcDir("src/testPerf/kotlin")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures { compose = true; buildConfig = true }

    packaging {
        resources.excludes += "/META-INF/{AL2.0,LGPL2.1}"
    }
}

composeCompiler {
    stabilityConfigurationFiles.add(layout.projectDirectory.file("compose-stability.conf"))
}

dependencies {
    implementation(project(":core"))
    implementation(platform(libs.compose.bom))
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.navigation.compose)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.compose.ui)
    implementation(libs.compose.material3)
    implementation(libs.compose.material.icons)
    // The perf build's self test: its plain logic is tested on the JVM (src/testPerf).
    testImplementation("junit:junit:4.13.2")
}
