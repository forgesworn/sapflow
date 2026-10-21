#!/bin/sh
set -eu
cd "$(dirname "$0")"
: "${ANDROID_HOME:=$HOME/Library/Android/sdk}"
export ANDROID_HOME
sapflow_output="$(cd ../.. && pwd)/target/mobile-android"
mkdir -p "$sapflow_output/kotlin/sessions"
exec ./gradlew --project-cache-dir "$sapflow_output/gradle-cache" \
  "-Pkotlin.project.persistent.dir=$sapflow_output/kotlin" \
  --no-daemon :app:verifyQualificationIsolation :app:testQualificationUnitTest \
  :app:assembleQualification :app:lintQualification "$@"
