pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}
dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}
rootProject.name = "ShardVeil"
// Two apps, one browser. `core` holds the shell they share; each app module
// supplies only its engine, and each produces its own APK.
include(":core")
include(":shard")
include(":veil")
