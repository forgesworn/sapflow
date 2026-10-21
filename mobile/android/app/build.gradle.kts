import java.io.File

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
        versionCode = 1
        versionName = "0.0.0"
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
    buildFeatures { compose = true }
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
    testImplementation("junit:junit:4.13.2")
}
