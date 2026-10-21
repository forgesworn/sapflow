import java.io.File
import java.net.URI

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

// Development-only override for a real package built from the same generated
// bindings and device ABI. Default remains the complete four-ABI SDK package.
val sapflowAarOverride = providers.gradleProperty("sapflowAar").orNull
val sapflowAarFile = if (sapflowAarOverride == null) {
    file("../../../target/native-packages/android/sapflow-0.0.0.aar")
} else {
    require(File(sapflowAarOverride).isAbsolute) { "sapflowAar must be an absolute path" }
    file(sapflowAarOverride)
}

android {
    namespace = "dev.forgesworn.sapflow.broadcaster"
    compileSdk = 36
    defaultConfig {
        applicationId = "dev.forgesworn.sapflow.broadcaster"
        minSdk = 29
        targetSdk = 36
        // This first connector package is ARM64 qualification; other ABIs require their own SDK/JNI gate.
        ndk { abiFilters += setOf("arm64-v8a") }
        versionCode = 1
        versionName = "0.0.0"
        fun literal(value: String) = "\"" + value.replace("\\", "\\\\").replace("\"", "\\\"") + "\""
        // Build-approved public routing only. No credential or arbitrary in-app profile import.
        val apiOrigin = providers.gradleProperty("sapflowV4vApiOrigin").orElse("").get()
        val playbackOrigin = providers.gradleProperty("sapflowV4vPlaybackOrigin").orElse("").get()
        val playbackPrefix = providers.gradleProperty("sapflowV4vPlaybackPrefix").orElse("").get()
        require(listOf(apiOrigin, playbackOrigin, playbackPrefix).all { it.isEmpty() } || listOf(apiOrigin, playbackOrigin, playbackPrefix).all { it.isNotEmpty() }) { "Supply the complete approved V4V profile or leave it disabled" }
        if (apiOrigin.isNotEmpty()) {
            for (value in listOf(apiOrigin, playbackOrigin)) {
                val uri = URI(value)
                require(value.all { it.code >= 32 } && uri.scheme == "https" && uri.host != null && uri.rawUserInfo == null && uri.rawQuery == null && uri.rawFragment == null && uri.rawPath in listOf("", "/")) { "V4V profile requires HTTPS origins" }
            }
            require(playbackPrefix.startsWith('/') && playbackPrefix.endsWith('/') && !playbackPrefix.contains("..") && !playbackPrefix.contains("//") && playbackPrefix.matches(Regex("/[A-Za-z0-9/_-]*"))) { "Invalid approved playback prefix" }
        }
        buildConfigField("String", "V4V_API_ORIGIN", literal(apiOrigin))
        buildConfigField("String", "V4V_PLAYBACK_ORIGIN", literal(playbackOrigin))
        buildConfigField("String", "V4V_PLAYBACK_PREFIX", literal(playbackPrefix))
    }
    buildTypes {
        create("qualification") {
            initWith(getByName("release"))
            isDebuggable = false
            signingConfig = signingConfigs.getByName("debug")
            matchingFallbacks += listOf("release")
        }
    }
    sourceSets {
        getByName("qualification") {
            // Internal qualification shares only the offline benchmark source
            // and resources. Its own manifest enables shell profiling.
            java.srcDir("src/debug/java")
            res.srcDir("src/debug/res")
        }
    }
    buildFeatures { compose = true; buildConfig = true }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    packaging { jniLibs.useLegacyPackaging = false }
}

tasks.register("verifyQualificationIsolation") {
    doLast {
        val qualification = android.buildTypes.getByName("qualification")
        val release = android.buildTypes.getByName("release")
        check(!qualification.isDebuggable)
        check(qualification.isMinifyEnabled == release.isMinifyEnabled)
        check(qualification.isShrinkResources == release.isShrinkResources)
        check(qualification.signingConfig == android.signingConfigs.getByName("debug"))
        val debugJava = file("src/debug/java").canonicalFile
        val debugResources = file("src/debug/res").canonicalFile
        val qualificationJava = file("src/qualification/java").canonicalFile
        val qualificationSources = android.sourceSets.getByName("qualification")
        check(qualificationJava in qualificationSources.java.srcDirs.map { it.canonicalFile })
        check(file("src/qualification/java/dev/forgesworn/sapflow/broadcaster/QualificationPoseApplication.kt").isFile)
        val qualificationManifest = file("src/qualification/AndroidManifest.xml").readText()
        check(".QualificationPoseApplication" in qualificationManifest)
        check("<receiver" !in qualificationManifest)
        check(debugJava in qualificationSources.java.srcDirs.map { it.canonicalFile })
        check(debugResources in qualificationSources.res.srcDirs.map { it.canonicalFile })
        for (name in listOf("main", "release", "debug")) {
            val sources = android.sourceSets.getByName(name)
            check(qualificationJava !in sources.java.srcDirs.map { it.canonicalFile })
            check(sources.java.srcDirs.none { directory ->
                directory.walkTopDown().any { it.name == "QualificationPoseApplication.kt" }
            })
            val manifest = file("src/$name/AndroidManifest.xml")
            check(!manifest.exists() || "QualificationPoseApplication" !in manifest.readText())
            if (name == "debug") continue
            check(debugJava !in sources.java.srcDirs.map { it.canonicalFile })
            check(debugResources !in sources.res.srcDirs.map { it.canonicalFile })
            check(sources.java.srcDirs.none { directory ->
                directory.walkTopDown().any { it.name == "FfiBenchmarkActivity.kt" }
            })
        }
    }
}

kotlin {
    compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) }
}

dependencies {
    implementation(files(sapflowAarFile))
    implementation("net.java.dev.jna:jna:5.19.1@aar")
    implementation(platform("androidx.compose:compose-bom:2024.12.01"))
    implementation("androidx.activity:activity-compose:1.9.3")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.material3:material3")
    implementation("org.jetbrains.kotlinx:kotlinx-serialization-json:1.7.3")
    // Newest Kotlin 2.2-metadata compatible release. 0.23/0.24 require Kotlin 2.3.
    implementation("fr.acinq.secp256k1:secp256k1-kmp-jvm:0.22.0")
    implementation("fr.acinq.secp256k1:secp256k1-kmp-jni-android:0.22.0")
    testRuntimeOnly("fr.acinq.secp256k1:secp256k1-kmp-jni-jvm:0.22.0")
    testImplementation("junit:junit:4.13.2")
}
