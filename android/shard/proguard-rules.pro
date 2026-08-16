# What R8 must not touch in a release build.
#
# The rest of the app is fair game to shrink and rename; these three things are
# reached by NAME from outside the Kotlin world, where a rename is a crash R8
# cannot see coming.

# 1. The JNI boundary. Rust binds to `Java_net_shard_Native_start` and friends by
#    mangled name — both the class name and the method names have to survive, so
#    the whole class is kept rather than just its `native` members.
-keep class net.shard.Native { *; }

# The generic native-method rule as well, for anything either app adds later.
-keepclasseswithmembernames,includedescriptorclasses class * {
    native <methods>;
}

# 2. The web bridge. Methods a page calls through `addJavascriptInterface` are
#    invoked by name from JavaScript; without this R8 renames them and every
#    long-press-to-download and every YouTube format lookup goes silent.
-keepclassmembers class * {
    @android.webkit.JavascriptInterface <methods>;
}

# 3. Media3/ExoPlayer resolves decoders and components partly by reflection. It
#    ships its own consumer rules, but pinning the surface keeps a shrink from
#    removing a renderer the device turns out to need.
-keep class androidx.media3.** { *; }
-dontwarn androidx.media3.**

# Line numbers, so a stack trace from a release build still points somewhere.
-keepattributes SourceFile,LineNumberTable
-renamesourcefileattribute SourceFile
