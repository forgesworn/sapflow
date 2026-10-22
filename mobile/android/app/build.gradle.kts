import com.android.build.gradle.BaseExtension
import java.io.ByteArrayInputStream
import java.nio.file.Files
import java.nio.file.LinkOption
import java.security.MessageDigest
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.util.Base64
import javax.xml.XMLConstants
import javax.xml.parsers.DocumentBuilderFactory
import javax.xml.transform.TransformerFactory
import javax.xml.transform.dom.DOMSource
import javax.xml.transform.stream.StreamResult
import java.io.File
import java.net.URI

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose")
}

// Read only the workspace package table, never a dependency/crate version.
// Native packaging uses this same double-quoted Cargo version as its filename.
fun workspacePackageVersion(manifest: String): String {
    var packageTable = false
    var tables = 0
    val versions = mutableListOf<String>()
    for (line in manifest.lineSequence()) {
        val value = line.trim()
        if (value.startsWith("[")) {
            packageTable = Regex("""^\[workspace\.package]\s*(?:#.*)?$""").matches(value)
            if (packageTable) tables++
        } else if (packageTable && Regex("""^version\s*=""").containsMatchIn(value)) {
            val match = Regex("""^version\s*=\s*"([^"]+)"\s*(?:#.*)?$""").matchEntire(value)
                ?: error("Expected a double-quoted workspace.package.version")
            versions += match.groupValues[1]
        }
    }
    require(tables == 1 && versions.size == 1) { "Expected one workspace.package.version" }
    val version = versions.single()
    require(version.length <= 128 && Regex("""[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?""").matches(version)) {
        "Expected a filename-safe workspace.package.version"
    }
    return version
}

// Development-only override for a real package built from the same generated
// bindings and device ABI. Default remains the complete four-ABI SDK package.
val sapflowAarOverride = providers.gradleProperty("sapflowAar").orNull
val sapflowAarFile = if (sapflowAarOverride == null) {
    val version = workspacePackageVersion(providers.fileContents(rootProject.layout.projectDirectory.file("../../Cargo.toml")).asText.get())
    file("../../../target/native-packages/android/sapflow-$version.aar")
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

// Public certificate and public routing only. This opt-in source set never
// changes release/default configuration or the platform TLS implementation.
val fixtureNames = listOf("CaFile", "ApiOrigin", "PlaybackOrigin", "PlaybackPrefix")
val fixtureValues = fixtureNames.associateWith { providers.gradleProperty("sapflowQualificationV4v$it").orElse("").get() }
require(fixtureValues.values.all { it.isEmpty() } || fixtureValues.values.all { it.isNotEmpty() }) {
    "Supply all four qualification V4V fixture properties or leave them disabled"
}
if (fixtureValues.values.any { it.isNotEmpty() }) {
    fun localOrigin(value: String): String {
        val uri = URI(value)
        require(value.matches(Regex("https://localhost(?::[1-9][0-9]{0,4})?/?")) &&
            uri.host == "localhost" && (uri.port == -1 || uri.port in 1..65535)) {
            "Qualification fixture origins must be HTTPS literal localhost"
        }
        return value.trimEnd('/')
    }
    val api = localOrigin(fixtureValues.getValue("ApiOrigin"))
    val playback = localOrigin(fixtureValues.getValue("PlaybackOrigin"))
    val prefix = fixtureValues.getValue("PlaybackPrefix")
    require(prefix.length <= 256 && prefix.startsWith('/') && prefix.endsWith('/') &&
        !prefix.contains("..") && !prefix.contains("//") && prefix.matches(Regex("/[A-Za-z0-9/_-]*"))) {
        "Invalid qualification playback prefix"
    }
    val ca = File(fixtureValues.getValue("CaFile"))
    fun publicCaBytes(): ByteArray {
        require(ca.isAbsolute && Files.isRegularFile(ca.toPath(), LinkOption.NOFOLLOW_LINKS)) {
            "Qualification fixture requires an absolute regular PUBLIC CA file (at most 16 KiB)"
        }
        return Files.newInputStream(ca.toPath(), LinkOption.NOFOLLOW_LINKS).use { input ->
            input.readNBytes(16_385).also {
                require(it.size in 1..16_384) { "PUBLIC CA input exceeds 16 KiB or is empty" }
            }
        }
    }
    val supplied = publicCaBytes()
    val der = if (supplied.first() == '-'.code.toByte()) {
        require(supplied.all { it.toInt() in 0..127 }) { "Invalid public CA PEM" }
        val pem = supplied.toString(Charsets.US_ASCII)
        val match = Regex("\\s*-----BEGIN CERTIFICATE-----\\s*([A-Za-z0-9+/=\\r\\n]+)\\s*-----END CERTIFICATE-----\\s*").matchEntire(pem)
        require(match != null) { "Supply exactly one public CA certificate, without keys or other PEM blocks" }
        Base64.getMimeDecoder().decode(match.groupValues[1])
    } else supplied
    val input = ByteArrayInputStream(der)
    val certificate = CertificateFactory.getInstance("X.509").generateCertificate(input) as X509Certificate
    require(input.available() == 0 && certificate.basicConstraints >= 0) { "Public trust input must be one X.509 CA certificate" }
    certificate.checkValidity()
    certificate.keyUsage?.let { require(it.size > 5 && it[5]) { "CA certificate does not permit certificate signing" } }
    val canonical = certificate.encoded
    require(canonical.contentEquals(der)) { "CA certificate must use canonical DER" }
    val digest = MessageDigest.getInstance("SHA-256").digest(canonical).joinToString("") { "%02x".format(it) }

    val android = extensions.getByType<BaseExtension>()
    val productionFields = android.defaultConfig.buildConfigFields.toMap()
    val releaseFields = android.buildTypes.getByName("release").buildConfigFields.toMap()
    android.buildTypes.getByName("qualification").apply {
        buildConfigField("String", "V4V_API_ORIGIN", "\"$api\"")
        buildConfigField("String", "V4V_PLAYBACK_ORIGIN", "\"$playback\"")
        buildConfigField("String", "V4V_PLAYBACK_PREFIX", "\"$prefix\"")
    }
    val output = layout.buildDirectory.dir("generated/qualification-v4v-fixture")
    val sourceManifest = file("src/qualification/AndroidManifest.xml")
    val generatedManifest = output.map { it.file("AndroidManifest.xml") }
    val generatedResources = output.map { it.dir("res") }
    val generate = tasks.register("generateQualificationV4vFixture") {
        inputs.file(ca)
        inputs.file(sourceManifest)
        inputs.properties(mapOf("api" to api, "playback" to playback, "prefix" to prefix, "caDerSha256" to digest))
        outputs.dir(output)
        doLast {
            // Certificate validation above is repeated on every Gradle invocation;
            // no key is accepted or copied. Only canonical public DER is emitted.
            check(publicCaBytes().contentEquals(supplied)) { "Public CA changed during this build" }
            val root = output.get().asFile
            val raw = File(root, "res/raw").apply { mkdirs() }
            val xml = File(root, "res/xml").apply { mkdirs() }
            File(raw, "sapflow_qualification_ca.der").writeBytes(canonical)
            File(xml, "sapflow_qualification_network_security.xml").writeText("""
                <network-security-config>
                    <base-config cleartextTrafficPermitted="false">
                        <trust-anchors><certificates src="system" /></trust-anchors>
                    </base-config>
                    <domain-config cleartextTrafficPermitted="false">
                        <domain includeSubdomains="false">localhost</domain>
                        <trust-anchors><certificates src="@raw/sapflow_qualification_ca" /></trust-anchors>
                    </domain-config>
                </network-security-config>
            """.trimIndent() + "\n")
            val factory = DocumentBuilderFactory.newInstance().apply {
                isNamespaceAware = true
                setFeature("http://apache.org/xml/features/disallow-doctype-decl", true)
                setAttribute(XMLConstants.ACCESS_EXTERNAL_DTD, "")
                setAttribute(XMLConstants.ACCESS_EXTERNAL_SCHEMA, "")
            }
            val document = factory.newDocumentBuilder().parse(sourceManifest)
            val application = document.getElementsByTagName("application")
            check(application.length == 1)
            // Keep the original qualification components unchanged.
            (application.item(0) as org.w3c.dom.Element).setAttributeNS(
                "http://schemas.android.com/apk/res/android", "android:networkSecurityConfig",
                "@xml/sapflow_qualification_network_security")
            val transformer = TransformerFactory.newInstance().apply {
                setAttribute(XMLConstants.ACCESS_EXTERNAL_DTD, "")
                setAttribute(XMLConstants.ACCESS_EXTERNAL_STYLESHEET, "")
            }.newTransformer()
            transformer.transform(DOMSource(document), StreamResult(generatedManifest.get().asFile))
        }
    }
    android.sourceSets.getByName("qualification").apply {
        manifest.srcFile(generatedManifest)
        res.srcDir(generatedResources)
    }
    tasks.configureEach {
        if (name == "preQualificationBuild") dependsOn(generate)
    }
    tasks.named("verifyQualificationIsolation") {
        doLast {
            check(android.defaultConfig.buildConfigFields == productionFields)
            check(android.buildTypes.getByName("release").buildConfigFields == releaseFields)
            for (name in listOf("main", "release", "debug")) {
                val sources = android.sourceSets.getByName(name)
                check(sources.manifest.srcFile != generatedManifest.get().asFile)
                check(generatedResources.get().asFile !in sources.res.srcDirs)
            }
        }
    }
}
