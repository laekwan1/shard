plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "net.sw.browser"
    compileSdk = 35

    defaultConfig {
        minSdk = 26
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
}

dependencies {
    // `api` rather than `implementation`: both apps subclass BrowserActivity and
    // so need these types on their own compile classpath.
    api("androidx.core:core-ktx:1.15.0")
    api("androidx.appcompat:appcompat:1.7.0")
    api("com.google.android.material:material:1.12.0")
    api("androidx.lifecycle:lifecycle-runtime-ktx:2.8.7")
    // ProxyController: points this app's web views at the local engine, which
    // is what removes the need for a VPN permission entirely.
    api("androidx.webkit:webkit:1.12.1")
    api("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.9.0")
    // The library's player. The platform MediaPlayer fails to decode some of
    // what YouTube hands out — high-bitrate VP9, AV1 — with a bare "-38"; Media3's
    // ExoPlayer plays what the device's codecs can and seeks to the exact frame.
    api("androidx.media3:media3-exoplayer:1.4.1")

    testImplementation("junit:junit:4.13.2")
}
