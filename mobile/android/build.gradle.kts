plugins {
    id("com.android.application") version "8.13.0" apply false
    id("org.jetbrains.kotlin.android") version "2.2.20" apply false
    id("org.jetbrains.kotlin.plugin.compose") version "2.2.20" apply false
}
// Keep every owned build/cache output inside the checkout's existing target/.
layout.buildDirectory.set(file("../../target/mobile-android/root"))
subprojects {
    layout.buildDirectory.set(rootProject.file("../../target/mobile-android/${project.name}"))
}
