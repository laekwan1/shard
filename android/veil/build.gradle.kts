import org.gradle.internal.os.OperatingSystem

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

// Emulators are x86_64. Opt in with `gradle :veil:assembleDebug -Pemu`.
val emulator = providers.gradleProperty("emu").isPresent
val abis = buildList {
    add("arm64-v8a")
    add("armeabi-v7a")
    if (emulator) add("x86_64")
}

android {
    namespace = "net.veil"
    compileSdk = 35

    defaultConfig {
        applicationId = "net.veil"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
        ndk { abiFilters += abis }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
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

    sourceSets["main"].jniLibs.srcDirs("src/main/jniLibs")
}

// The tor library is built against a newer Kotlin than this project uses, and
// drags its standard library along. Pinning keeps one compiler and one stdlib
// rather than a build that half-compiles against each.
configurations.all {
    resolutionStrategy {
        force("org.jetbrains.kotlin:kotlin-stdlib:2.0.21")
        force("org.jetbrains.kotlin:kotlin-stdlib-jdk7:2.0.21")
        force("org.jetbrains.kotlin:kotlin-stdlib-jdk8:2.0.21")
    }
}

dependencies {
    // The browser shell, shared with Shard.
    implementation(project(":core"))

    // Tor itself, from the Guardian Project — the same build Orbot runs. Using
    // the upstream artifact rather than a private copy is what keeps this from
    // falling behind the network it has to blend into.
    // Pinned to the last release that does not demand compileSdk 37: the 0.4.9
    // line requires an SDK newer than this project targets, and moving the
    // whole build forward for one dependency is a larger change than it is
    // worth. 0.4.8.21 is a current stable tor.
    implementation("info.guardianproject:tor-android:0.4.8.21.2")
    implementation("androidx.localbroadcastmanager:localbroadcastmanager:1.1.0")
}

/** The Rust half: share link in, sing-box configuration out. */
val cargoNdk by tasks.registering(Exec::class) {
    group = "build"
    description = "Compile veil-mobile for Android via cargo-ndk"

    val manifest = rootProject.file("../crates/veil-mobile/Cargo.toml")
    val output = file("src/main/jniLibs")
    val cargo = if (OperatingSystem.current().isWindows) "cargo.exe" else "cargo"

    inputs.file(manifest)
    inputs.dir(rootProject.file("../crates"))

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
