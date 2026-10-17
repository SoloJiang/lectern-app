// Sanitized sample: Kotlin DSL settings script (Gradle 8).
pluginManagement {
    includeBuild("build-logic")
    repositories {
        google()
        mavenCentral()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
}

rootProject.name = "acme-server"

include(
    ":api",
    ":service:billing",
    ":service:search",
)
include(":tools:migrations")
project(":tools:migrations").projectDir = file("db/migrations")

if (rootProject.name == "acme-server") {
    include(":optional")
}
