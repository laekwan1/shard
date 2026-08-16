import org.gradle.internal.os.OperatingSystem
import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

// Emulators are x86_64. Building that ABI doubles the work and ships a slice no
// phone will ever load, so it is opt-in: `gradle :shard:assembleDebug -Pemu`.
val emulator = providers.gradleProperty("emu").isPresent
val abis = buildList {
    add("arm64-v8a")
    add("armeabi-v7a")
    if (emulator) add("x86_64")
}

// The release signing key, read from a file that is never committed. Absent —
// on a fresh clone or a machine without the key — a release build still runs;
// it just comes out unsigned, so the missing key is a warning, not a wall.
val keystoreProps = Properties().apply {
    val f = rootProject.file("keystore.properties")
    if (f.exists()) f.inputStream().use { load(it) }
}
val hasKeystore = keystoreProps.getProperty("storeFile") != null

// Monotonic by construction: one more commit is one higher code, so an update
// can never present a version the phone thinks it already has. versionName
// carries the same count so the number a user sees moves with every build.
fun gitCount(): Int = try {
    val p = ProcessBuilder("git", "rev-list", "--count", "HEAD")
        .directory(rootProject.projectDir).start()
    p.inputStream.bufferedReader().readText().trim().toInt().also { p.waitFor() }
} catch (e: Exception) {
    1
}

android {
    namespace = "net.shard"
    compileSdk = 35

    defaultConfig {
        applicationId = "net.shard"
        minSdk = 26
        targetSdk = 35
        versionCode = gitCount()
        versionName = "0.1.${gitCount()}"
        ndk { abiFilters += abis }
    }

    signingConfigs {
        if (hasKeystore) {
            create("release") {
                storeFile = rootProject.file(keystoreProps.getProperty("storeFile"))
                storePassword = keystoreProps.getProperty("storePassword")
                keyAlias = keystoreProps.getProperty("shardAlias")
                keyPassword = keystoreProps.getProperty("keyPassword")
                // v2 covers the whole file against tampering (min SDK 26 needs
                // no v1); v3 carries the key's identity so the signer can be
                // rotated later without every phone treating it as a new app.
                enableV1Signing = false
                enableV2Signing = true
                enableV3Signing = true
            }
        }
    }

    buildTypes {
        release {
            // Shrink and obfuscate: a smaller download, and a release the
            // curious cannot read straight out of the APK. The keep rules that
            // hold the JNI and web bridges together are in proguard-rules.pro.
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
            if (hasKeystore) signingConfig = signingConfigs.getByName("release")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
    buildFeatures {
        viewBinding = true
    }
    // cargo-ndk writes the shared libraries straight into the jniLibs layout.
    sourceSets["main"].jniLibs.srcDirs("src/main/jniLibs")
}

dependencies {
    // The browser shell, shared with Veil.
    implementation(project(":core"))
}

/**
 * Build the Rust engine for Android and drop the .so files where Gradle packs
 * them. Keeping this in the build rather than a separate script means the APK
 * can never ship a stale engine.
 */
val cargoNdk by tasks.registering(Exec::class) {
    group = "build"
    description = "Compile shard-mobile for Android via cargo-ndk"

    val manifest = rootProject.file("../crates/shard-mobile/Cargo.toml")
    val output = file("src/main/jniLibs")
    val cargo = if (OperatingSystem.current().isWindows) "cargo.exe" else "cargo"

    inputs.file(manifest)
    inputs.dir(rootProject.file("../crates"))
    outputs.dir(output)

    commandLine(
        buildList {
            add(cargo); add("ndk")
            add("--manifest-path"); add(manifest.absolutePath)
            abis.forEach { add("-t"); add(it) }
            add("-o"); add(output.absolutePath)
            add("build"); add("--release")
        }
    )
}

tasks.matching { it.name.startsWith("merge") && it.name.endsWith("JniLibFolders") }.configureEach {
    dependsOn(cargoNdk)
}
