import org.gradle.internal.os.OperatingSystem

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

android {
    namespace = "net.shard"
    compileSdk = 35

    defaultConfig {
        applicationId = "net.shard"
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
