plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "io.github.cokefish.despegate"
    compileSdk = 34

    defaultConfig {
        applicationId = "io.github.cokefish.despegate"
        minSdk = 28
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0"
    }

    buildTypes {
        // The debug build can be taken off a phone from a computer
        // (`adb shell dpm remove-active-admin`), which only works for
        // test-only apps. The release build cannot: its one way out is the
        // app's own uninstall.
        debug {
            manifestPlaceholders["testOnly"] = "true"
        }
        release {
            manifestPlaceholders["testOnly"] = "false"
            isMinifyEnabled = false
            // A personal build: signed with this machine's debug key, which
            // is also what later updates must be signed with.
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
}

dependencies {
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.json:json:20240303")
}
