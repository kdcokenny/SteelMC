plugins {
    java
    id("com.google.protobuf") version "0.10.0"
    id("com.gradleup.shadow") version "9.6.1"
}

group = "dev.steelmc"
version = "0.1.0+mc26.2"

repositories {
    mavenCentral()
    maven("https://repo.papermc.io/repository/maven-public/")
}

dependencyLocking {
    lockAllConfigurations()
}

dependencies {
    compileOnly("dev.folia:folia-api:26.2.build.1-beta")
    testImplementation("dev.folia:folia-api:26.2.build.1-beta")
    implementation(platform("io.grpc:grpc-bom:1.83.1"))
    implementation("io.grpc:grpc-netty-shaded")
    implementation("io.grpc:grpc-protobuf")
    implementation("io.grpc:grpc-stub")
    compileOnly("javax.annotation:javax.annotation-api:1.3.2")
    testImplementation(platform("org.junit:junit-bom:6.0.0"))
    testImplementation("org.junit.jupiter:junit-jupiter")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

java {
    toolchain.languageVersion = JavaLanguageVersion.of(25)
}

sourceSets {
    main {
        proto.srcDir("../../steel-worldgen-service/proto")
    }
    test {
        resources.srcDir("../../steel-worldgen-service/test_assets")
    }
}

protobuf {
    protoc { artifact = "com.google.protobuf:protoc:3.25.9" }
    plugins {
        create("grpc") { artifact = "io.grpc:protoc-gen-grpc-java:1.83.1" }
    }
    generateProtoTasks {
        all().configureEach { plugins { create("grpc") } }
    }
}

val pluginVersion = version.toString()
val sourceRevision = providers.environmentVariable("STEEL_WORLDGEN_BUILD_ID")
    .orElse("local-unpublished-build")
val sourceUrl = providers.environmentVariable("STEEL_WORLDGEN_SOURCE_URL")
    .orElse("https://github.com/Steel-Foundation/SteelMC")
tasks.processResources {
    inputs.property("version", pluginVersion)
    inputs.property("sourceRevision", sourceRevision)
    inputs.property("sourceUrl", sourceUrl)
    filesMatching("plugin.yml") { expand("version" to pluginVersion) }
    filesMatching("META-INF/SteelMC-SOURCE.txt") {
        expand("sourceRevision" to sourceRevision.get(), "sourceUrl" to sourceUrl.get())
    }
    from("../../LICENSE") {
        into("META-INF")
        rename { "LICENSE-SteelMC-AGPL-3.0-or-later.txt" }
    }
}

tasks.compileJava {
    options.release = 25
    options.encoding = "UTF-8"
    options.compilerArgs.addAll(listOf("-Xlint:all", "-Werror"))
}

tasks.test { useJUnitPlatform() }

tasks.shadowJar {
    archiveClassifier = ""
    duplicatesStrategy = DuplicatesStrategy.EXCLUDE
    filesMatching("META-INF/services/**") {
        duplicatesStrategy = DuplicatesStrategy.INCLUDE
    }
    mergeServiceFiles()
}

tasks.jar { enabled = false }
tasks.assemble { dependsOn(tasks.shadowJar) }
